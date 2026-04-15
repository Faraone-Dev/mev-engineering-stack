/**
 * Lock-Free MPSC Queue for Opportunity Pipeline
 * Multiple producers (detectors), single consumer (executor)
 */

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <stdalign.h>
#include <stdatomic.h>

#ifdef _WIN32
#include <malloc.h>
#define aligned_alloc(align, size) _aligned_malloc(size, align)
#define aligned_free(ptr) _aligned_free(ptr)
#else
#define aligned_free(ptr) free(ptr)
#endif

#define QUEUE_CAPACITY 4096  // Must be power of 2

typedef struct {
    atomic_uintptr_t data;
} queue_slot_t;

typedef struct {
    queue_slot_t* slots;
    size_t capacity;
    size_t mask;
    
    // Aligned to separate cache lines
    alignas(64) atomic_size_t head;  // Consumer reads from here
    alignas(64) atomic_size_t tail;  // Producers write here
} mev_queue_t;

/**
 * Create a new queue
 */
mev_queue_t* mev_queue_create(size_t capacity) {
    if (capacity & (capacity - 1)) {
        // Not power of 2, round up
        size_t n = 1;
        while (n < capacity) n <<= 1;
        capacity = n;
    }
    
    mev_queue_t* q = (mev_queue_t*)aligned_alloc(64, sizeof(mev_queue_t));
    if (!q) return NULL;
    
    q->slots = (queue_slot_t*)calloc(capacity, sizeof(queue_slot_t));
    if (!q->slots) {
        free(q);
        return NULL;
    }
    
    q->capacity = capacity;
    q->mask = capacity - 1;
    atomic_store(&q->head, 0);
    atomic_store(&q->tail, 0);
    
    return q;
}

/**
 * Destroy queue
 */
void mev_queue_destroy(mev_queue_t* q) {
    if (q) {
        free(q->slots);
        free(q);
    }
}

/**
 * Push item to queue (producer, lock-free)
 * Returns 0 on success, -1 if full
 */
int mev_queue_push(mev_queue_t* q, void* item) {
    size_t tail = atomic_load_explicit(&q->tail, memory_order_relaxed);
    
    while (1) {
        size_t head = atomic_load_explicit(&q->head, memory_order_acquire);
        
        if (tail - head >= q->capacity) {
            return -1; // Full
        }
        
        if (atomic_compare_exchange_weak_explicit(
                &q->tail, &tail, tail + 1,
                memory_order_release, memory_order_relaxed)) {
            // Got our slot
            size_t idx = tail & q->mask;
            atomic_store_explicit(&q->slots[idx].data, (uintptr_t)item, memory_order_release);
            return 0;
        }
        // CAS failed, tail was updated, retry
    }
}

/**
 * Pop item from queue (consumer, single-threaded)
 * Returns item or NULL if empty
 */
void* mev_queue_pop(mev_queue_t* q) {
    size_t head = atomic_load_explicit(&q->head, memory_order_relaxed);
    size_t tail = atomic_load_explicit(&q->tail, memory_order_acquire);
    
    if (head >= tail) {
        return NULL; // Empty
    }
    
    size_t idx = head & q->mask;
    void* item = (void*)atomic_load_explicit(&q->slots[idx].data, memory_order_acquire);
    
    // Clear slot
    atomic_store_explicit(&q->slots[idx].data, 0, memory_order_relaxed);
    
    // Advance head
    atomic_store_explicit(&q->head, head + 1, memory_order_release);
    
    return item;
}

/**
 * Try to pop without blocking
 */
void* mev_queue_try_pop(mev_queue_t* q) {
    return mev_queue_pop(q);
}

/**
 * Get queue size (approximate)
 */
size_t mev_queue_size(mev_queue_t* q) {
    size_t tail = atomic_load(&q->tail);
    size_t head = atomic_load(&q->head);
    return tail - head;
}

/**
 * Check if empty
 */
int mev_queue_empty(mev_queue_t* q) {
    return mev_queue_size(q) == 0;
}

/**
 * Batch pop for efficiency
 */
size_t mev_queue_pop_batch(mev_queue_t* q, void** items, size_t max_items) {
    size_t count = 0;
    
    while (count < max_items) {
        void* item = mev_queue_pop(q);
        if (!item) break;
        items[count++] = item;
    }
    
    return count;
}
