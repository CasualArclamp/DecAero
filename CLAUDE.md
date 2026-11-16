<!-- made by CasualArclamp -->
# DecAero — working notes

Read [`docs/DESIGN.md`](docs/DESIGN.md) first: scope, decisions, milestones.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python scripts/gen_ambe_tables.py ../jaero-ref/libaeroambe/mbelib-master > crates/decaero-ambe/src/tables.rs
```

## House rules

- GPL-3.0-or-later. Credit borrowed code at the function or module, and in NOTICE
  (JAERO and libaeroambe are MIT, mbelib is ISC, DecSAT is GPL-3).
- Never commit IQ captures, recordings, decoded audio, aircraft databases,
  reference clones (`..\jaero-ref`) or built exes.
- JAERO is the reference for framing: cite its function next to constants and layouts.
- Every file starts with a low-key `made by CasualArclamp` comment in its own syntax.
- Receive only. Nothing here keys a transmitter.
- The user has a strong DSP background and is newer to Rust: explain Rust idioms, not DSP.
- DSP claims (sensitivity above all) are measured on the test modulator before they are written down.
