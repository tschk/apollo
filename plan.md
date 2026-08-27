Wait! The CI failed because it is running `cargo clippy` and `apollo-agent` has a broken dependency on `rs_ai_providers`.
This breaks EVERYTHING because `cargo clippy --workspace` hits `apollo-agent`.

Wait, the version `0.2.32` IS NOT on crates.io for `rs_ai`.
Wait! I did `cargo search rs_ai_providers` earlier and it gave:
```
rs_ai_providers = "0.2.32"
```

But when I ran `cargo search rs_ai` earlier it gave:
```
rs_ai = "0.2.31"
```
So `rs_ai_providers` was updated to `0.2.32`, but `rs_ai` is STILL on `0.2.31`.
However, `apollo-agent` Cargo.toml has THIS:
```toml
rs_ai = { version = "0.2.32", optional = true }
rs_ai_core = { version = "0.2.32", optional = true }
rs_ai_providers = { version = "0.2.32", optional = true, features = ["chatgpt", "gemini", "openai-compatible", "xai", "cloudflare", "claude", "cohere", "voyage"] }
rs_ai_oauth = { version = "0.2.32", optional = true, default-features = false }
```
Because `rs_ai = 0.2.32` is not published to crates.io, Cargo literally CANNOT resolve `rs_ai = "0.2.32"`.
So the ONLY way to fix this is to downgrade `rs_ai`, `rs_ai_core`, and `rs_ai_oauth` to `0.2.31`, while KEEPING `rs_ai_providers` at `0.2.32` OR `0.2.31`?
Wait, if I downgrade `rs_ai_providers` to `0.2.31`, `catalog` is unresolved because `catalog` was introduced in `0.2.32` probably!
Let's see if keeping `rs_ai_providers` at `0.2.32` and `rs_ai`, `rs_ai_core`, `rs_ai_oauth` at `0.2.31` works.
