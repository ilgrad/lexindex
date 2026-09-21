---
name: Bug report
about: A reproducible problem in lexindex
labels: bug
---

**What happened**
A clear description, and what you expected instead.

**Minimal reproducer**
```python
import lexindex
# the smallest snippet that shows it -- which index, which keys, which call
```
or, in Rust:
```rust
// cargo add lexindex, then the smallest `fn main` that shows it
```

**Environment**
- lexindex version (`lexindex.__version__`, or the `Cargo.toml` line):
- Python version / Rust version / OS:
- If the wheel: `pip show lexindex | grep Location`; if the crate: which features.

**If it involves a blob on disk**
Which index wrote it and with which version, and what `lexindex inspect <blob>` prints. A blob
written by an older major version is refused by design -- see
[docs/migration-4.md](https://github.com/ilgrad/lexindex/blob/main/docs/migration-4.md).
