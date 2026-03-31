/**
 * MEV Protocol - C Hot Path
 * Ultra-fast Keccak256 implementation
 * 
 * Optimized for minimal latency in MEV extraction
 */

#include "keccak.h"
#include <string.h>

/* Keccak-256 constants */
#define KECCAK_ROUNDS 24

static const uint64_t RC[24] = {
    0x0000000000000001ULL, 0x0000000000008082ULL,
    0x800000000000808aULL, 0x8000000080008000ULL,
    0x000000000000808bULL, 0x0000000080000001ULL,
    0x8000000080008081ULL, 0x8000000000008009ULL,
    0x000000000000008aULL, 0x0000000000000088ULL,
    0x0000000080008009ULL, 0x000000008000000aULL,
    0x000000008000808bULL, 0x800000000000008bULL,
    0x8000000000008089ULL, 0x8000000000008003ULL,
    0x8000000000008002ULL, 0x8000000000000080ULL,
    0x000000000000800aULL, 0x800000008000000aULL,
    0x8000000080008081ULL, 0x8000000000008080ULL,
    0x0000000080000001ULL, 0x8000000080008008ULL
};

static const int ROTATION[24] = {
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14,
    27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44
};

static const int PI[24] = {
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4,
    15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1
};

/* Rotate left */
static inline uint64_t rotl64(uint64_t x, int n) {
    return (x << n) | (x >> (64 - n));
}

/* Keccak-f[1600] permutation */
static void keccak_f(uint64_t state[25]) {
    uint64_t temp, C[5], D[5];
    int i, j, round;

    for (round = 0; round < KECCAK_ROUNDS; round++) {
        /* Theta step */
        for (i = 0; i < 5; i++) {
            C[i] = state[i] ^ state[i + 5] ^ state[i + 10] ^ state[i + 15] ^ state[i + 20];
        }
        
        for (i = 0; i < 5; i++) {
            D[i] = C[(i + 4) % 5] ^ rotl64(C[(i + 1) % 5], 1);
        }
        
        for (i = 0; i < 25; i++) {
            state[i] ^= D[i % 5];
        }

        /* Rho and Pi steps */
        temp = state[1];
        for (i = 0; i < 24; i++) {
            j = PI[i];
            uint64_t t = state[j];
            state[j] = rotl64(temp, ROTATION[i]);
            temp = t;
        }

        /* Chi step */
        for (j = 0; j < 25; j += 5) {
            uint64_t t0 = state[j];
            uint64_t t1 = state[j + 1];
            uint64_t t2 = state[j + 2];
            uint64_t t3 = state[j + 3];
            uint64_t t4 = state[j + 4];
            
            state[j]     = t0 ^ ((~t1) & t2);
            state[j + 1] = t1 ^ ((~t2) & t3);
            state[j + 2] = t2 ^ ((~t3) & t4);
            state[j + 3] = t3 ^ ((~t4) & t0);
            state[j + 4] = t4 ^ ((~t0) & t1);
        }

        /* Iota step */
        state[0] ^= RC[round];
    }
}

/**
 * Compute Keccak-256 hash
 * 
 * @param input Input data
 * @param input_len Length of input data
 * @param output Output buffer (must be at least 32 bytes)
 * @return 0 on success, -1 on error
 */
int mev_keccak256(const uint8_t *input, size_t input_len, uint8_t *output) {
    if (!input || !output) {
        return -1;
    }

    uint64_t state[25] = {0};
    uint8_t temp[136]; /* Rate for Keccak-256 */
    size_t rate = 136;
    size_t i, offset = 0;

    /* Absorb phase — use memcpy to avoid UB on unaligned input */
    while (input_len >= rate) {
        for (i = 0; i < rate / 8; i++) {
            uint64_t lane;
            memcpy(&lane, input + i * 8, 8);
            state[i] ^= lane;
        }
        keccak_f(state);
        input += rate;
        input_len -= rate;
    }

    /* Padding */
    memset(temp, 0, rate);
    memcpy(temp, input, input_len);
    temp[input_len] = 0x01;  /* Keccak padding (not SHA3!) */
    temp[rate - 1] |= 0x80;

    for (i = 0; i < rate / 8; i++) {
        uint64_t lane;
        memcpy(&lane, temp + i * 8, 8);
        state[i] ^= lane;
    }
    keccak_f(state);

    /* Squeeze phase - output 256 bits */
    memcpy(output, state, 32);

    return 0;
}

/**
 * Compute Keccak-256 for Ethereum address
 * Takes last 20 bytes of hash of public key
 */
int mev_keccak256_address(const uint8_t *pubkey, size_t pubkey_len, uint8_t *address) {
    uint8_t hash[32];
    
    if (mev_keccak256(pubkey, pubkey_len, hash) != 0) {
        return -1;
    }
    
    /* Take last 20 bytes */
    memcpy(address, hash + 12, 20);
    return 0;
}

/**
 * Compute function selector (first 4 bytes of keccak256)
 */
uint32_t mev_function_selector(const char *signature) {
    uint8_t hash[32];
    size_t len = strlen(signature);
    
    if (mev_keccak256((const uint8_t*)signature, len, hash) != 0) {
        return 0;
    }
    
    return ((uint32_t)hash[0] << 24) | 
           ((uint32_t)hash[1] << 16) | 
           ((uint32_t)hash[2] << 8) | 
           (uint32_t)hash[3];
}
