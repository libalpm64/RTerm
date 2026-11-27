# Vendored Pageant

This is `pageant` 0.2.5 from `warp-tech/russh`, upstream commit
`183a22e8f8f490fa09b6eb7bd044ef99d8c4414f`.

The small local patch unifies its Windows bindings with RTerm's existing
`windows` 0.61.3 dependency, avoiding a second generated Windows API crate and
its `windows-core` 0.62 dependency branch. The compatibility adjustments are:

- Use `windows::core::PSTR` instead of a separately versioned
  `windows-strings` dependency.
- Use `windows::core::Error::from_win32()`, the 0.61 API equivalent of
  `Error::from_thread()` (both read `GetLastError()` and convert it to an
  HRESULT).

The upstream crate is Apache-2.0 licensed. Revisit this patch when updating
Pageant or `russh`; prefer removing the local patch if upstream aligns its
Windows dependency with the application's version.
