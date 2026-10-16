use groove_verify::run_all_formal_proofs;

fn main() {
    println!("========================================================================================================================");
    println!("  GROOVE FORMAL VERIFICATION & MODEL-CHECKING SUITE");
    println!("  Engine: Stateright Explicit-State Model Checker (TLA+) + Exhaustive Algebraic Provers + Proptest Shrinking");
    println!("========================================================================================================================");
    println!(
        "{:<18} | {:<32} | {:<12} | {:>12} | {:>9} | Theorem / Invariants",
        "PROOF ID", "SUBSYSTEM", "STATUS", "STATES/CASES", "TIME (ms)"
    );
    println!("-------------------+----------------------------------+--------------+--------------+-----------+-----------------------");

    let report = run_all_formal_proofs();
    for cert in &report.certificates {
        let status = if cert.verified {
            "VERIFIED ✓"
        } else {
            "FAILED ✗"
        };
        println!(
            "{:<18} | {:<32} | {:<12} | {:>12} | {:>9.2} | {}",
            cert.id,
            cert.subsystem,
            status,
            cert.states_or_cases_explored,
            cert.elapsed_ms,
            cert.theorem
        );
    }

    println!("========================================================================================================================");
    println!(
        "  SUMMARY: {}/{} formal proofs VERIFIED  |  {} reachable states & proof cases checked in {:.2} ms",
        report.certificates.iter().filter(|c| c.verified).count(),
        report.certificates.len(),
        report.total_states_or_cases,
        report.total_elapsed_ms
    );
    println!("========================================================================================================================");

    if !report.all_verified() {
        std::process::exit(1);
    }
}
