# bariera-esp32

Open a gate or garage door by **calling or texting a phone number**.

Written in Rust (`no_std`) with [esp-hal](https://github.com/esp-rs/esp-hal) and [Embassy](https://embassy.dev).

## How it works

```
 phone ──GSM──▶ SIM800L ──UART──▶ ESP32-C3 ──GPIO/RMT──▶ 433 MHz TX ──RF──▶ gate
```

1. The SIM800L registers on the 2G network and pushes events over UART (`+CLIP` for calls, `+CMT` for SMS).
2. The SIM task parses each event, hangs up calls (`ATH`), and checks the sender against the allowed list.
3. If the sender is allowed, it sends an `RfCommand` through an `embassy_sync::Channel`.
4. The RF side transmits the captured pulse train with the RMT peripheral (1 µs resolution, 3 repeats with 40 ms gaps).

## Configuration

Secrets and site-specific values live in `src/config.rs`, which is **git-ignored**.

```sh
cp src/config.example.rs src/config.rs
```

Then edit it:

```rust

// Numbers allowed to open the gate (international format, as sent by +CLIP / +CMT)
pub const ALLOWED_NUMBERS: &[(&str, &str)] = &[("Matei Oprea", "+40700000000")];

// Raw pulse train captured from the original remote, in µs:
// positive = high, negative = low
pub const GATE_CODE: &[i16] = &[837, -6542, 553, -1234 /* ... */];

```

## License

MIT
