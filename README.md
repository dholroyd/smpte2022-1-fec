smpte2022-1-fec
===============

Implementation of the core state-machine for Forward Error Correction specificed by _SMPTE 2022-1_ (previously
_Pro-MPEG Code of Practice #3_).

[![crates.io version](https://img.shields.io/crates/v/smpte2022-1-fec.svg)](https://crates.io/crates/smpte2022-1-fec)
[![Documentation](https://docs.rs/smpte2022-1-fec/badge.svg)](https://docs.rs/smpte2022-1-fec)
![Unstable API](https://img.shields.io/badge/API_stability-unstable-yellow.svg)

This create does not make any assumptions about the application's IO model, and should be able to work with MIO /
Tokio / others.  On the other hand, this crate does not provide any of those integrations itself, so you will need
to provide your own UDP packet processing logic.  (Other crates may provide such integrations in future.)
