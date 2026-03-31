/**
 * Lock-Free Memory Pool for Zero-Allocation Hot Path
 * Pre-allocates buffers to avoid malloc during execution
 */

#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include <stdatomic.h>

#ifdef _WIN32
#include <windows.h>
#include <malloc.h>
#define aligned_alloc(align, size) _aligned_malloc(size, align)
#define aligned_free(ptr) _aligned_free(ptr)
#else
#include <sys/mman.h>
#define aligned_free(ptr) free(ptr)
#endif

#define POOL_BLOCK_SIZE 4096
#define POOL_MAX_BLOCKS 1024

typedef struct {
    void* blocks[POOL_MAX_BLOCKS];
    atomic_uint head;
    atomic_uint tail;
    atomic_uint count;
    size_t block_size;
} mev_memory_pool_t;

static mev_memory_pool_t g_tx_pool;      // For transaction buffers
static mev_memory_pool_t g_calldata_pool; // For calldata
static mev_memory_pool_t g_result_pool;   // For results
static int g_pools_initialized = 0;

/**
 * Allocate aligned memory
 */
static void* alloc_aligned(size_t size, size_t alignment) {
#ifdef _WIN32
    return _aligned_malloc(size, alignment);
#else
    void* ptr;
    if (posix_memalign(&ptr, alignment, size) != 0) return NULL;
    return ptr;
#endif
}

/**
 * Initialize a memory pool
 */
static int pool_init(mev_memory_pool_t* pool, size_t block_size, size_t initial_blocks) {
    pool->block_size = block_size;
    atomic_store(&pool->head, 0);
    atomic_store(&pool->tail, 0);
    atomic_store(&pool->count, 0);
    
    memset(pool->blocks, 0, sizeof(pool->blocks));
    
    // Pre-allocate blocks
    for (size_t i = 0; i < initial_blocks && i < POOL_MAX_BLOCKS; i++) {
        void* block = alloc_aligned(block_size, 64); // Cache-line aligned
        if (!block) return -1;
        pool->blocks[i] = block;
        atomic_fetch_add(&pool->count, 1);
        atomic_store(&pool->tail, i + 1);
    }
    
    return 0;
}

/**
 * Get a block from pool (lock-free)
 */
static void* pool_get(mev_memory_pool_t* pool) {
    while (1) {
        unsigned int head = atomic_load(&pool->head);
        unsigned int tail = atomic_load(&pool->tail);
        
        if (head >= tail) {
            // Pool empty - allocate new (slow path)
            return alloc_aligned(pool->block_size, 64);
        }
        
        if (atomic_compare_exchange_weak(&pool->head, &head, head + 1)) {
            void* block = pool->blocks[head % POOL_MAX_BLOCKS];
            return block;
        }
        // CAS failed, retry
    }
}

/**
 * Return block to pool (lock-free)
 *
 * Write the block pointer into the slot BEFORE bumping the tail so that
 * consumers never observe a slot with a stale/NULL pointer.  We use
 * acquire/release ordering on tail to guarantee the store to blocks[]
 * is visible before a consumer reads the updated tail.
 */
static void pool_put(mev_memory_pool_t* pool, void* block) {
    if (!block) return;
    
    unsigned int count = atomic_load_explicit(&pool->count, memory_order_relaxed);
    if (count >= POOL_MAX_BLOCKS) {
        // Pool full - free block
#ifdef _WIN32
        _aligned_free(block);
#else
        free(block);
#endif
        return;
    }
    
    unsigned int tail = atomic_load_explicit(&pool->tail, memory_order_relaxed);
    unsigned int new_tail;
    do {
        new_tail = tail + 1;
        /* Try to claim the slot at `tail`. If another thread raced us,
           the CAS fails and we retry with the updated tail value. */
    } while (!atomic_compare_exchange_weak_explicit(
        &pool->tail, &tail, new_tail,
        memory_order_acq_rel, memory_order_relaxed));
    
    /* Now we own the slot — write the block pointer. The release in the
       CAS above ensures this store is ordered before any subsequent
       load of tail from a consumer. */
    pool->blocks[tail % POOL_MAX_BLOCKS] = block;
}

/*
 * Public API
 */

int mev_pools_init(void) {
    if (g_pools_initialized) return 0;
    
    if (pool_init(&g_tx_pool, 512, 256) != 0) return -1;       // 256 tx buffers
    if (pool_init(&g_calldata_pool, 2048, 128) != 0) return -1; // 128 calldata buffers
    if (pool_init(&g_result_pool, 256, 512) != 0) return -1;    // 512 result buffers
    
    g_pools_initialized = 1;
    return 0;
}

void* mev_alloc_tx(void) {
    return pool_get(&g_tx_pool);
}

void mev_free_tx(void* ptr) {
    pool_put(&g_tx_pool, ptr);
}

void* mev_alloc_calldata(void) {
    return pool_get(&g_calldata_pool);
}

void mev_free_calldata(void* ptr) {
    pool_put(&g_calldata_pool, ptr);
}

void* mev_alloc_result(void) {
    return pool_get(&g_result_pool);
}

void mev_free_result(void* ptr) {
    pool_put(&g_result_pool, ptr);
}

/**
 * Batch allocate for parallel processing
 */
int mev_alloc_batch(void** ptrs, size_t count, size_t size) {
    mev_memory_pool_t* pool;
    
    if (size <= 256) pool = &g_result_pool;
    else if (size <= 512) pool = &g_tx_pool;
    else pool = &g_calldata_pool;
    
    for (size_t i = 0; i < count; i++) {
        ptrs[i] = pool_get(pool);
        if (!ptrs[i]) {
            // Rollback
            for (size_t j = 0; j < i; j++) {
                pool_put(pool, ptrs[j]);
            }
            return -1;
        }
    }
    
    return 0;
}

void mev_free_batch(void** ptrs, size_t count, size_t size) {
    mev_memory_pool_t* pool;
    
    if (size <= 256) pool = &g_result_pool;
    else if (size <= 512) pool = &g_tx_pool;
    else pool = &g_calldata_pool;
    
    for (size_t i = 0; i < count; i++) {
        pool_put(pool, ptrs[i]);
    }
}

/**
 * Get pool stats for monitoring
 */
void mev_pool_stats(size_t* tx_avail, size_t* calldata_avail, size_t* result_avail) {
    *tx_avail = atomic_load(&g_tx_pool.tail) - atomic_load(&g_tx_pool.head);
    *calldata_avail = atomic_load(&g_calldata_pool.tail) - atomic_load(&g_calldata_pool.head);
    *result_avail = atomic_load(&g_result_pool.tail) - atomic_load(&g_result_pool.head);
}
