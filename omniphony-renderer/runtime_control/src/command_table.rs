//! Declared tables of the control addresses that are not options.
//!
//! The live options are declared in `renderer::options` and reached through
//! the generic setters and their aliases. Everything else a client sends is
//! a command (save, apply, upload, recenter, a profile switch), transient
//! state (test signals, mutes, the manual head pose, the overlay), a
//! per-client subscription (metering, diagnostics, gain tables) or a value
//! whose shape the registry does not take (per speaker, per family, a point
//! list, a value on the engine rather than the live params): see
//! `docs/live-options-registry.md`, "Outside the registry". Each layer that
//! handles some of them declares them in one table of [`Command`]s instead
//! of a chain of address comparisons, so the addresses it answers to can be
//! listed and checked (in the OSC contract's catalogue, claimed by no other
//! table, no registry alias).

/// The addresses a [`Command`] answers to.
#[derive(Debug, Clone, Copy)]
pub enum Addr {
    Exact(&'static str),
    /// Several addresses handled alike; the handler reads `msg.addr` when it
    /// needs to tell them apart.
    Any(&'static [&'static str]),
    /// Every address under a prefix (a path that carries an index), tried
    /// after every exact address of the table.
    Prefix(&'static str),
}

impl Addr {
    fn matches_exactly(self, addr: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == addr,
            Self::Any(all) => all.contains(&addr),
            Self::Prefix(_) => false,
        }
    }

    /// The whole addresses this answers to (none for a prefix).
    pub fn exact(&self) -> &[&'static str] {
        match self {
            Self::Exact(exact) => std::slice::from_ref(exact),
            Self::Any(all) => all,
            Self::Prefix(_) => &[],
        }
    }
}

/// One entry of a table: an address and its handler, whose type the table's
/// layer chooses.
pub struct Command<F> {
    pub addr: Addr,
    pub run: F,
}

impl<F> Command<F> {
    pub const fn exact(addr: &'static str, run: F) -> Self {
        Self {
            addr: Addr::Exact(addr),
            run,
        }
    }

    pub const fn any(addrs: &'static [&'static str], run: F) -> Self {
        Self {
            addr: Addr::Any(addrs),
            run,
        }
    }

    pub const fn prefix(prefix: &'static str, run: F) -> Self {
        Self {
            addr: Addr::Prefix(prefix),
            run,
        }
    }
}

/// The handler of `addr` in `table`: an exact address first, then a prefix.
pub fn find<F: Copy>(table: &[Command<F>], addr: &str) -> Option<F> {
    table
        .iter()
        .find(|command| command.addr.matches_exactly(addr))
        .or_else(|| {
            table.iter().find(
                |command| matches!(command.addr, Addr::Prefix(prefix) if addr.starts_with(prefix)),
            )
        })
        .map(|command| command.run)
}

/// Every whole address of `table`.
pub fn addresses<F>(table: &[Command<F>]) -> impl Iterator<Item = &'static str> + '_ {
    table
        .iter()
        .flat_map(|command| command.addr.exact().iter().copied())
}

/// The checks every table must pass: each whole address is in the contract's
/// catalogue, none is claimed twice (within the table or by `others`), and
/// none is a registry option's alias. Returns what fails, one line each.
pub fn problems<F>(table: &[Command<F>], others: &[&dyn Fn() -> Vec<&'static str>]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let elsewhere: Vec<&str> = others.iter().flat_map(|other| other()).collect();
    for addr in addresses(table) {
        if !crate::osc_contract::ALL_CONTROL.contains(&addr) {
            problems.push(format!("{addr}: not in osc_contract::ALL_CONTROL"));
        }
        if !seen.insert(addr) {
            problems.push(format!("{addr}: twice in the table"));
        }
        if elsewhere.contains(&addr) {
            problems.push(format!("{addr}: also claimed by another layer"));
        }
        if renderer::options::find_by_legacy_addr(addr).is_some() {
            problems.push(format!("{addr}: a registry option's alias"));
        }
    }
    problems
}

/// The core's tables and the process commands, for a layer that checks its
/// own table against them.
pub fn core_addresses() -> Vec<&'static str> {
    addresses(crate::osc::SIMPLE_CONTROL_COMMANDS)
        .chain(addresses(crate::live_control::LIVE_CONTROL_COMMANDS))
        .chain(crate::command::PROCESS_COMMANDS.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_tables_are_declared_in_the_contract_and_claimed_once() {
        let live = || addresses(crate::live_control::LIVE_CONTROL_COMMANDS).collect();
        let simple = || addresses(crate::osc::SIMPLE_CONTROL_COMMANDS).collect();
        let process = || crate::command::PROCESS_COMMANDS.to_vec();
        let mut found = problems(crate::osc::SIMPLE_CONTROL_COMMANDS, &[&live, &process]);
        found.extend(problems(
            crate::live_control::LIVE_CONTROL_COMMANDS,
            &[&simple, &process],
        ));
        assert!(found.is_empty(), "{found:#?}");
    }

    #[test]
    fn an_exact_address_wins_over_a_prefix() {
        let table: &[Command<u8>] = &[Command::prefix("/a/", 1), Command::exact("/a/b", 2)];
        assert_eq!(find(table, "/a/b"), Some(2));
        assert_eq!(find(table, "/a/c"), Some(1));
        assert_eq!(find(table, "/b"), None);
    }
}
