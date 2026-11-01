// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

fn main() {
    let sys = foldl::sys::Sys::current();
    std::process::exit(foldl::cli::run(std::env::args_os().collect(), sys));
}
