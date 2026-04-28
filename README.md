# Rterm

A lightweight terminal emulator built with Rust, wry, and xterm.js.

**Supported Platforms:** macOS, Linux, BSD (UNIX-like systems only)

**Note:** Windows is not supported. This application uses POSIX APIs (`poll()`) for I/O multiplexing and does not implement IOCP or Windows thread affinity. If you want Windows support, you can implement a spin lock in a forked process with ~1000 thread wake-ups per thread. But let's be honest, your Surface Laptop will last 90 minutes at best with that approach.
