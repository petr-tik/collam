# collam
[![Build Status](https://travis-ci.org/gcarq/collam.svg?branch=master)](https://travis-ci.org/gcarq/collam) [![Coverage Status](https://coveralls.io/repos/github/gcarq/collam/badge.svg?branch=master)](https://coveralls.io/github/gcarq/collam?branch=master)

A naive and thread safe general-purpose allocator written in Rust built with `#[no_std]`.
This project started as an experiment to get comfortable with `#[no_std]` environments and `unsafe` Rust.
This library is currently *NOT* stable and I'm sure there are plenty of bugs, be warned!

## A note on its state
Exposed POSIX functions: `malloc`, `calloc`, `realloc`, `free`, `malloc_usable_size`, `mallopt`.
It is currently stable with a lot of tested programs using `LD_PRELOAD`, however it does not implement Rusts `GlobalAlloc` yet.

## Tested platforms
[x] Linux x86_64

## Implementation details
Bookkeeping is currently done with an intrusive doubly linked list.
The overhead for each use allocated block is 16 bytes whereas only 12 bytes of them are used.

## Performance
In regards of memory usage/overhead it is comparable to dlmalloc with tested applications,
however the performance is not there yet.

## Testing collam in C/POSIX environment
Make sure you have Rust nightly.
Manually overwrite default allocator:
```bash
$ cargo build --features posix --release
$ LD_PRELOAD="$(pwd)/target/release/libcollam.so" kwrite
```
Or use the test script in the root folder:
```bash
$ ./scripts/test.sh kwrite
```
There are some more helper scripts for debugging, profiling, etc. See `scripts/` folder.


## Execute tests
Tests are not thread safe, make sure to force 1 thread only!
```bash
$ cargo test --all-features -- --test-threads 1
```

## TODO:
* Set correct `crate-type` to use it as [GlobalAlloc](https://doc.rust-lang.org/beta/std/alloc/trait.GlobalAlloc.html) within Rust
* Proper Page handling
* mmap support
* Thread-local allocation
* Logarithmic-time complexity allocation
* Support for different architectures
* Proper logging