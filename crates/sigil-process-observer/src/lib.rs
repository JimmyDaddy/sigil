//! RFC-0071: sigil-process-observer contract-only scaffold (R71.1).
//!
//! R71.1 freezes the observation service/verifier factory contract: runtime composes the
//! same-instance service and verifier, and never implements or replaces the verifier itself.
//! Real birth-identity and quiescence probes arrive using sigil-process primitives in R71.4.

/// Narrow contract marker bound for R71.1; the service/verifier pair is implemented in R71.4.
#[derive(Debug)]
pub struct HostProcessObservationFactoryV1 {
    pub factory_hash: String,
}
