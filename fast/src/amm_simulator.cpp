// amm_simulator.cpp — translation unit for amm_simulator.h
//
// This file exists solely to force the compiler to produce a concrete object
// file for the header-only AMM simulator.  All logic lives in amm_simulator.h;
// including it here causes the C ABI symbols (extern "C" blocks) to be emitted
// into the static library that Rust links against.
//
// Compile flag required: -std=c++20 (GCC/Clang) or /std:c++20 (MSVC)

#include "amm_simulator.h"
