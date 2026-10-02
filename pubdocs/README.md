# Solapp

Solapp is the proposed Rust application and host coordination layer for the
Sol libraries. It follows Solworker's project conventions and is intended to
compose Solworker execution and Solcache ownership through their public APIs.

Research covers event and service loops, frame phases, loading transitions,
owner-context application and dependency-ordered shutdown. The crate is an
initial scaffold: no application API, platform backend or dependency has been
selected. Public application types will use the `SA` prefix.
