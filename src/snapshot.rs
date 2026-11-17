// vim: ts=4 et

//! Taking due snapshots and sweeping expired claims -- the "claims"
//! lifecycle model from cryo.md, "Snapshot behaviors" (g). Claims from
//! a time- or counter-based policy are only ever dropped by their own
//! policy's expiry, in `expire_sweep`. An `indefinite` claim is never
//! dropped on its own -- only `cryo:limit`/`cryo:reserve` pressure
//! (`apply_limit`/`apply_reserve`) may strip one, and only when doing
//! so would actually free real disk space, i.e. the snapshot's other
//! claims (if any) are left completely alone.
//!
//! Work is dispatched one thread per pool (`apply_due_actions`),
//! mirroring `discover::discover`'s own per-pool concurrency: a slow
//! or busy pool's zfs calls shouldn't hold up any other pool's
//! snapshots. This is also the natural unit `cryo:reserve` pressure
//! cleanup needs anyway, since picking "the oldest eligible snapshot
//! anywhere in the pool" requires exactly the whole pool's worth of
//! datasets that one worker thread already owns.

use std::collections::HashMap;
use std::thread;

use crate::discover::{self, DatasetState, ManagedSnapshotState, Model};
use crate::error::Result;
use crate::policy::{self, Policy, Reserve, Retention};
use crate::replicate;
use crate::snapname;
use crate::target::Target;
use crate::timestamp::Timestamp;
use crate::zfs;

#[derive(Debug, Default)]
pub struct ActionResult {
    pub taken: Vec<String>,
    pub destroyed: Vec<String>,
    pub replicated: Vec<String>,
    pub errors: Vec<String>,
}

impl ActionResult {
    fn merge(&mut self, other: ActionResult) {
        self.taken.extend(other.taken);
        self.destroyed.extend(other.destroyed);
        self.replicated.extend(other.replicated);
        self.errors.extend(other.errors);
    }
}

/// Takes whatever's due and expires whatever's lapsed, across every
/// dataset in `model`, mutating it to match. Shared by the standalone
/// `cryo run` path and the daemon's own timer/on-demand-run path.
pub fn apply_due_actions(model: &mut Model, now: Timestamp) -> ActionResult {
    let mut by_pool: HashMap<String, HashMap<String, DatasetState>> = HashMap::new();

    for name in model.datasets.keys().cloned().collect::<Vec<_>>() {
        let state = model.datasets.remove(&name).expect("just listed this key");
        by_pool
            .entry(discover::pool_of(&name).to_string())
            .or_default()
            .insert(name, state);
    }

    let handles: Vec<_> = by_pool
        .into_iter()
        .map(|(pool, datasets)| {
            let reserve = model.pool_reserve.get(&pool).copied();
            thread::spawn(move || apply_due_actions_for_pool(&pool, datasets, reserve, now))
        })
        .collect();

    let mut result = ActionResult::default();

    for handle in handles {
        match handle.join() {
            Ok((datasets, pool_result)) => {
                model.datasets.extend(datasets);
                result.merge(pool_result);
            }
            Err(_) => result.errors.push("a pool worker thread panicked".to_string()),
        }
    }

    result
}

fn apply_due_actions_for_pool(
    pool: &str,
    mut datasets: HashMap<String, DatasetState>,
    reserve: Option<Reserve>,
    now: Timestamp,
) -> (HashMap<String, DatasetState>, ActionResult) {
    let mut result = ActionResult::default();
    let mut names: Vec<String> = datasets.keys().cloned().collect();
    names.sort();

    for name in &names {
        let state = datasets.get_mut(name).expect("just listed this key");

        match take_due_snapshot(name, state, now) {
            Ok(Some(full_name)) => result.taken.push(full_name),
            Ok(None) => {}
            Err(e) => result.errors.push(format!("{name}: {e}")),
        }

        match expire_sweep(name, state, now) {
            Ok(destroyed) => result.destroyed.extend(destroyed),
            Err(e) => result.errors.push(format!("{name}: {e}")),
        }

        match apply_limit(name, state) {
            Ok(destroyed) => result.destroyed.extend(destroyed),
            Err(e) => result.errors.push(format!("{name}: {e}")),
        }

        for target in &state.targets {
            match replicate::one_shot(name, target) {
                Ok(outcome) => result.replicated.push(outcome.message),
                Err(e) => result.errors.push(format!("{name} -> {}: {e}", target_label(target))),
            }
        }
    }

    if let Some(reserve) = reserve {
        match apply_reserve(pool, reserve, &mut datasets) {
            Ok(destroyed) => result.destroyed.extend(destroyed),
            Err(e) => result.errors.push(format!("{pool}: {e}")),
        }
    }

    (datasets, result)
}

fn target_label(target: &Target) -> &str {
    if target.name.is_empty() { "(default)" } else { &target.name }
}

pub fn due_policies(state: &DatasetState, now: Timestamp) -> Vec<&Policy> {
    state
        .policies
        .iter()
        .filter(|p| is_due(p, state, now))
        .collect()
}

fn is_due(policy: &Policy, state: &DatasetState, now: Timestamp) -> bool {
    match state.last_fired.get(&policy.name) {
        None => true,
        Some(last) => last.elapsed_secs_until(now) >= policy.frequency.minutes() as i64 * 60,
    }
}

/// Takes one snapshot satisfying every policy due on `dataset` right
/// now, if any are due. Updates `state` in place to reflect it.
pub fn take_due_snapshot(
    dataset: &str,
    state: &mut DatasetState,
    now: Timestamp,
) -> Result<Option<String>> {
    let due_names: Vec<String> = due_policies(state, now)
        .into_iter()
        .map(|p| p.name.clone())
        .collect();

    if due_names.is_empty() {
        return Ok(None);
    }

    let full_name = snapname::scheduled_name(dataset, now);
    zfs::create_snapshot(&full_name)?;
    zfs::set_property(&full_name, policy::CLAIMED_BY_PROPERTY, &due_names.join(","))?;

    for name in &due_names {
        zfs::set_property(dataset, &policy::last_property_for(name), &now.format())?;
        state.last_fired.insert(name.clone(), now);
    }

    state.snapshots.push(ManagedSnapshotState {
        full_name: full_name.clone(),
        instant: false,
        taken_at: now,
        claimed_by: due_names,
    });

    Ok(Some(full_name))
}

/// Drops any claim whose policy's retention has lapsed for a given
/// snapshot, destroying any snapshot left with no claims at all.
/// Returns the full names of whatever got destroyed.
pub fn expire_sweep(_dataset: &str, state: &mut DatasetState, now: Timestamp) -> Result<Vec<String>> {
    let mut surviving: Vec<Vec<String>> = state
        .snapshots
        .iter()
        .map(|s| s.claimed_by.clone())
        .collect();

    for policy in &state.policies {
        match policy.retention {
            Retention::Indefinite => continue,
            Retention::Duration(span) => {
                let max_age_secs = span.minutes() as i64 * 60;
                for (i, snap) in state.snapshots.iter().enumerate() {
                    let claimed = snap.claimed_by.iter().any(|n| n == &policy.name);
                    let expired = snap.taken_at.elapsed_secs_until(now) > max_age_secs;
                    if claimed && expired {
                        surviving[i].retain(|n| n != &policy.name);
                    }
                }
            }
            Retention::Count(limit) => {
                let mut claiming: Vec<(usize, Timestamp)> = state
                    .snapshots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.claimed_by.iter().any(|n| n == &policy.name))
                    .map(|(i, s)| (i, s.taken_at))
                    .collect();

                claiming.sort_by_key(|(_, taken_at)| *taken_at);
                claiming.reverse(); // newest first

                for (i, _) in claiming.into_iter().skip(limit as usize) {
                    surviving[i].retain(|n| n != &policy.name);
                }
            }
        }
    }

    let mut destroyed = Vec::new();
    let mut kept = Vec::new();

    for (i, snap) in state.snapshots.drain(..).enumerate() {
        if surviving[i].is_empty() {
            zfs::destroy_snapshot(&snap.full_name)?;
            destroyed.push(snap.full_name);
        } else if surviving[i] != snap.claimed_by {
            zfs::set_property(&snap.full_name, policy::CLAIMED_BY_PROPERTY, &surviving[i].join(","))?;
            kept.push(ManagedSnapshotState {
                claimed_by: surviving[i].clone(),
                ..snap
            });
        } else {
            kept.push(snap);
        }
    }

    state.snapshots = kept;
    Ok(destroyed)
}

/// `cryo:limit`: if the dataset plus its snapshots exceeds the limit,
/// strip indefinite claims oldest-first (destroying whatever that
/// empties out) until back under it, or until there's nothing left
/// pressure is allowed to touch. Dataset-scoped, unlike `apply_reserve`.
fn apply_limit(dataset: &str, state: &mut DatasetState) -> Result<Vec<String>> {
    let Some(limit) = state.limit else { return Ok(Vec::new()) };
    let mut destroyed = Vec::new();

    while state.used_with_snapshots() > limit {
        let Some(index) = oldest_indefinite_index(&state.policies, &state.snapshots) else {
            break; // over the limit, but nothing left we're allowed to strip
        };

        if let Some(full_name) = strip_indefinite_claim(&state.policies, &mut state.snapshots, index)? {
            destroyed.push(full_name);
            let (referenced, used_by_snapshots) = zfs::dataset_usage(dataset)?;
            state.referenced = referenced;
            state.used_by_snapshots = used_by_snapshots;
        }
        // Otherwise the claim was stripped but another claim still
        // holds the snapshot: no space was freed, and the next
        // iteration's oldest_indefinite_index skips it since it has
        // no indefinite claim left to strip.
    }

    Ok(destroyed)
}

/// `cryo:reserve`: pool-wide, across every dataset passed in. `trigger`
/// is the free-space floor that has to be crossed to start cleaning up
/// at all; `target` is how far back up to clean before stopping (the
/// same value for a fixed reserve; `high` for a `high:low` pair, so
/// there's a buffer against immediately re-triggering).
fn apply_reserve(
    pool: &str,
    reserve: Reserve,
    datasets: &mut HashMap<String, DatasetState>,
) -> Result<Vec<String>> {
    let (trigger, target) = match reserve {
        Reserve::Fixed(floor) => (floor, floor),
        Reserve::HighLow { high, low } => (low, high),
    };

    if zfs::pool_free(pool)? >= trigger {
        return Ok(Vec::new());
    }

    let mut destroyed = Vec::new();

    while zfs::pool_free(pool)? < target {
        let Some((dataset, index)) = oldest_indefinite_across_pool(datasets) else {
            break; // under the reserve, but nothing left we're allowed to strip
        };

        let state = datasets.get_mut(&dataset).expect("came from this same map");

        if let Some(full_name) = strip_indefinite_claim(&state.policies, &mut state.snapshots, index)? {
            destroyed.push(full_name);
        }
    }

    Ok(destroyed)
}

/// The oldest snapshot (by index) that still carries at least one
/// claim from a currently-configured `indefinite` policy -- the only
/// kind of claim pressure cleanup is ever allowed to touch. A claim
/// whose policy has since been removed entirely is left alone too:
/// with no policy left to check, we can't positively confirm it was
/// indefinite, and the safer failure mode is to leave it be.
fn oldest_indefinite_index(policies: &[Policy], snapshots: &[ManagedSnapshotState]) -> Option<usize> {
    snapshots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.claimed_by.iter().any(|name| is_indefinite_claim(policies, name)))
        .min_by_key(|(_, s)| s.taken_at)
        .map(|(i, _)| i)
}

fn oldest_indefinite_across_pool(datasets: &HashMap<String, DatasetState>) -> Option<(String, usize)> {
    datasets
        .iter()
        .filter_map(|(name, state)| {
            let index = oldest_indefinite_index(&state.policies, &state.snapshots)?;
            Some((name.clone(), index, state.snapshots[index].taken_at))
        })
        .min_by_key(|(_, _, taken_at)| *taken_at)
        .map(|(name, index, _)| (name, index))
}

fn is_indefinite_claim(policies: &[Policy], claim_name: &str) -> bool {
    policies
        .iter()
        .any(|p| p.name == claim_name && p.retention == Retention::Indefinite)
}

/// Drops every indefinite claim from the snapshot at `index`, leaving
/// every other claim completely untouched, and destroys it if that
/// empties the claim list out. Returns the destroyed name, if any.
fn strip_indefinite_claim(
    policies: &[Policy],
    snapshots: &mut Vec<ManagedSnapshotState>,
    index: usize,
) -> Result<Option<String>> {
    let snap = &mut snapshots[index];
    snap.claimed_by.retain(|name| !is_indefinite_claim(policies, name));

    if snap.claimed_by.is_empty() {
        let full_name = snap.full_name.clone();
        zfs::destroy_snapshot(&full_name)?;
        snapshots.remove(index);
        Ok(Some(full_name))
    } else {
        zfs::set_property(&snap.full_name, policy::CLAIMED_BY_PROPERTY, &snap.claimed_by.join(","))?;
        Ok(None)
    }
}
