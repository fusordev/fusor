use fusor_bytecode::VerifiedControlFlow;

/// Renders every retained field of a staged control-flow certificate.
///
/// The verifier's derived `Debug` representation intentionally includes its
/// private instruction-boundary bitmap as well as all public certificate
/// state. Keeping the rendering behind one helper gives mechanical module
/// moves a byte-for-byte characterization test without exposing new authority
/// through the production API.
pub fn snapshot_verified_control_flow(verified: &VerifiedControlFlow) -> String {
    format!("{verified:?}\n")
}
