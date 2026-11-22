# AWS Runtime 1.10.2 unsigned aws-chunked framing

Upstream reviewed: 1.10.2

Published crates.io archive `aws-runtime-1.10.2.crate` has SHA-256
`9a0cc2144962cc5e358467b16e3d82981b1d424a7c3d34602be0036103814a1a`.
Its Cargo VCS metadata records `b964fc77de60b0a492ff788c40bb66524fd26a44`
at `sdk/aws-runtime` in [aws-sdk-rust](https://github.com/awslabs/aws-sdk-rust).

`src/content_encoding/body/http_body_1_x.rs` writes exact-size unsigned data
frames directly from the segmented cursor. Uppercase chunk-size digits use
stack storage. Full and short final chunks share the same writer. Signed chunks
continue to coalesce data for signing, and the existing trailer and length
checks own their original transitions. The framing regression checks aligned
and fragmented full and short chunks and the checksum terminator.

Retire this source patch and its Cargo, image, profile, and verification
entries together when upstream provides equivalent framing and stream
semantics without the intermediate fragmented copy or output growth.
