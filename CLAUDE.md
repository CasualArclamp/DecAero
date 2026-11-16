<!-- made by CasualArclamp -->
# DecAero — working notes

Read [`docs/DESIGN.md`](docs/DESIGN.md) first: scope, decisions, milestones.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --release -p decaero-gui -- IQ/areo.wav --center 1545.0            # the GUI on a recording (CMake on PATH for libopus)
cargo run --release -p decaero-gui -- FILE.wav --screenshot out/x.png --after 20 [--select HZ] [--fast]   # unattended look
cargo run --release -p decaero-engine --example headless -- FILE.wav [CENTRE_HZ] [--no-follow] [--add HZ:WIDTH]   # no window
cargo run --release -p decaero-engine --example net_send -- FILE.wav sdrpp://127.0.0.1:5259   # stand-in SDR++ server
cargo run --release -p decaero-gui -- --net sdrpp://127.0.0.1:5259 --center 1545.0   # live from an SDR++ server
cargo run --release -p decaero-dsp --example aerorx -- FILE.wav [--at HZ[:slow]] [--units]   # bench receiver
cargo run --release -p decaero-dsp --example bench    # speed of the hot loops (measure before and after)
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
- The radios on this machine may be held by the user's own apps: never open
  one (`--radio`, Start radio) without asking, and never kill what holds it.
- Never commit screenshots of real traffic (aircraft, messages): `out/` and
  `docs/images/` are ignored.
- The user has a strong DSP background and is newer to Rust: explain Rust idioms, not DSP.
- DSP claims (sensitivity above all) are measured on the test modulator before they are written down.
