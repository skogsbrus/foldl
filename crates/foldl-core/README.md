# foldl-core

The pure core of [foldl](https://github.com/skogsbrus/foldl), which runs
code when a shell enters or leaves a folder.

This crate does no I/O. It holds foldl's state, the events that change it
and a step function, `step(Machine, Event) -> (Machine, Effects)`. It also
has the environment diffs, manifest validation and shell rendering. The
`foldl` crate runs the effects and feeds their results back in as events.

It exists so that `foldl` can be tested as a pure fold. Its API follows
what `foldl` needs and may change in any 0.x release. To use foldl,
install the [`foldl`](https://crates.io/crates/foldl) crate.

MIT licensed.
