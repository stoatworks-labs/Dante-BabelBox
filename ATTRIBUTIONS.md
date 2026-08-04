# Attributions

Dante BabelBox is built on other people's work. This file lists what that work is, who did
it, and what it is doing here.

It is generated — the master lists live in the `stoatworks-backend` repo and are
pushed out by `scripts/sync-attributions.py`. Edit it there, not here.

## Code we derived from other people's work

Someone else solved this first, and this project would not exist in its current form without their work.

### network-audio-controller (netaudio-core) — Chris Ritsen

<https://github.com/chris-ritsen/network-audio-controller>  
Licence: Unlicense

Audinate's Dante ARC control protocol is undocumented. crates/discovery/src/dante_control.rs is a reimplementation rather than a port — no code was copied — but every constant, record layout and test in it is cross-checked against that crate's own known-answer unit tests. The wire format was documented and byte-tested there first, and guessing it from nothing would have been a different project.

## Third-party code this project uses

Libraries, SDKs and frameworks the project is built on or bundles.

### The Rust crate ecosystem

<https://crates.io>  
Licence: predominantly MIT or Apache-2.0  
Copyright: the individual crate authors

Cargo dependencies, resolved and pinned in Cargo.lock.

Async runtimes, protocol codecs, serialisation and GUI toolkits. The exact set and versions for any build are in that repo's Cargo.lock, which is the authoritative list.

The full transitive dependency set for any build is pinned in this repo's lockfile,
which is the authoritative list. What is named above is the layers a reader would
want to know about, not every package that has ever been resolved.

## Getting this wrong

If your work is here and the description is inaccurate, the licence is wrong, or you would rather not be listed — open an issue and it will be fixed.
