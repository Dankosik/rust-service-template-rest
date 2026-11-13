# tonic 0.14.8 client response observation

Upstream reviewed: 0.14.8

The published archive has SHA-256
`cd1fa9b1a51188f3266a823208078315811269d06d272c3a61be0b9a194194ff`
and records source revision `541379959bf197057a7c04eeca7f1d976f5ba7c6`.
README links to upstream workspace files point to the upstream repository.

The generated clients use tonic's decoder and cardinality rules after the
underlying HTTP service returns. The patch lets that dispatcher tell the
service when an RPC completes and with which code. It also makes status-detail
decoding return a status when invalid input would otherwise unwind. It adds no
dependency and leaves generated client signatures and wire codecs intact.

Retire the patch when the selected native client provides a supported
completion hook covering response validation, decoded streams, and unary
cardinality. The infra-grpc client status and metrics regression must pass on
the replacement. Remove the profile, Cargo and image wiring with the patch.
