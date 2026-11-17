// vim: ts=4 et

//! Builds an in-memory `Model` of every policy, scheduling state, and
//! cryo-managed snapshot (with its claims) across every imported pool,
//! by walking zfs properties -- inheritance already resolved by zfs
//! itself, since `zfs get -r` reports each dataset's effective value
//! regardless of where it was actually set.

use std::collections::HashMap;
use std::thread;

use crate::error::{CryoError, Result};
use crate::policy::{self, Policy, Reserve};
use crate::snapname;
use crate::target::{self, Target};
use crate::timestamp::Timestamp;
use crate::zfs;

type Datasets = HashMap<String, DatasetState>;

/// The pool a dataset belongs to, i.e. the first path component.
pub fn pool_of(dataset: &str) -> &str {
    dataset.split('/').next().unwrap_or(dataset)
}

#[derive(Debug, Clone, Default)]
pub struct DatasetState {
    pub policies: Vec<Policy>,
    /// Where each policy's value actually came from ("local",
    /// "default", "inherited from <dataset>", ...) as reported by
    /// `zfs get`'s own `source` column, keyed by policy name.
    pub policy_sources: HashMap<String, String>,
    pub last_fired: HashMap<String, Timestamp>,
    pub snapshots: Vec<ManagedSnapshotState>,

    /// `cryo:limit`, if set on this dataset.
    pub limit: Option<u64>,
    /// `referenced` and `usedbysnapshots`, fetched only when `limit`
    /// is set -- together, "the dataset plus all its snapshots" per
    /// cryo.md, without descendant datasets' usage mixed in.
    pub referenced: u64,
    pub used_by_snapshots: u64,

    /// `cryo:target[-<name>]`: where this dataset's cryo snapshots
    /// get replicated to.
    pub targets: Vec<Target>,
}

impl DatasetState {
    pub fn used_with_snapshots(&self) -> u64 {
        self.referenced + self.used_by_snapshots
    }
}

#[derive(Debug, Clone)]
pub struct ManagedSnapshotState {
    pub full_name: String,
    pub instant: bool,
    pub taken_at: Timestamp,
    pub claimed_by: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Model {
    pub datasets: HashMap<String, DatasetState>,
    /// `cryo:reserve`, keyed by pool name, set on the pool's root
    /// dataset.
    pub pool_reserve: HashMap<String, Reserve>,
}

struct PoolDiscovery {
    datasets: Datasets,
    reserve: Option<Reserve>,
}

/// Discovers every pool concurrently, one thread per pool -- the same
/// principle as `snapshot::apply_due_actions`: a pool's own zfs calls
/// are the unit of work that shouldn't block any other pool's. Wall
/// clock becomes the slowest single pool's scan rather than the sum
/// of all of them (on one real host, that cut a ~26s scan dominated
/// by one 665-dataset pool down to that pool's own ~13s).
pub fn discover() -> Result<Model> {
    let handles: Vec<_> = zfs::list_pools()?
        .into_iter()
        .map(|pool| {
            thread::spawn(move || discover_pool(&pool).map(|d| (pool.clone(), d)).map_err(|e| format!("{pool}: {e}")))
        })
        .collect();

    let mut model = Model::default();
    let mut errors = Vec::new();

    for handle in handles {
        match handle.join() {
            Ok(Ok((pool, discovery))) => {
                model.datasets.extend(discovery.datasets);
                if let Some(reserve) = discovery.reserve {
                    model.pool_reserve.insert(pool, reserve);
                }
            }
            Ok(Err(msg)) => errors.push(msg),
            Err(_) => errors.push("a pool discovery thread panicked".to_string()),
        }
    }

    if !errors.is_empty() {
        return Err(CryoError::Command(errors.join("; ")));
    }

    Ok(model)
}

fn discover_pool(pool: &str) -> Result<PoolDiscovery> {
    let mut datasets = Datasets::new();
    let reserve = load_properties(&mut datasets, pool)?;
    load_snapshots(&mut datasets, pool)?;
    Ok(PoolDiscovery { datasets, reserve })
}

fn load_properties(datasets: &mut Datasets, pool: &str) -> Result<Option<Reserve>> {
    // referenced/usedbysnapshots are native properties, always
    // present with real values -- collected for every dataset here,
    // but only copied onto a DatasetState below for ones that
    // actually have cryo:limit set, so a dataset with no cryo
    // involvement at all never gets an entry just for carrying size
    // info nothing will use.
    let mut sizes: HashMap<String, (u64, u64)> = HashMap::new();
    let mut reserve = None;

    for prop in zfs::all_properties(pool)? {
        match prop.name.as_str() {
            "referenced" => {
                sizes.entry(prop.dataset.clone()).or_default().0 = prop.value.parse().unwrap_or(0);
                continue;
            }
            "usedbysnapshots" => {
                sizes.entry(prop.dataset.clone()).or_default().1 = prop.value.parse().unwrap_or(0);
                continue;
            }
            _ => {}
        }

        if prop.value == "-" {
            continue;
        }

        if let Some(name) = policy::policy_name_from_property(&prop.name) {
            match policy::parse_policy_value(name, &prop.value) {
                Ok(p) => {
                    let state = datasets.entry(prop.dataset.clone()).or_default();
                    state.policy_sources.insert(name.to_string(), prop.source.clone());
                    state.policies.push(p);
                }
                Err(e) => eprintln!("cryo: {}: {e}", prop.dataset),
            }
            continue;
        }

        if let Some(name) = policy::last_policy_name_from_property(&prop.name) {
            match Timestamp::parse(&prop.value) {
                Ok(ts) => {
                    datasets
                        .entry(prop.dataset)
                        .or_default()
                        .last_fired
                        .insert(name.to_string(), ts);
                }
                Err(e) => eprintln!("cryo: {}: {e}", prop.dataset),
            }
            continue;
        }

        if prop.name == policy::LIMIT_PROPERTY {
            match policy::parse_limit_value(&prop.value) {
                Ok(limit) => datasets.entry(prop.dataset.clone()).or_default().limit = Some(limit),
                Err(e) => eprintln!("cryo: {}: {e}", prop.dataset),
            }
            continue;
        }

        if let Some(name) = target::target_name_from_property(&prop.name) {
            match target::parse_target_value(name, &prop.value) {
                Ok(t) => datasets.entry(prop.dataset.clone()).or_default().targets.push(t),
                Err(e) => eprintln!("cryo: {}: {e}", prop.dataset),
            }
            continue;
        }

        // cryo:reserve is pool-wide: only the pool's root dataset's
        // value counts, even though it's inheritable like everything
        // else and so technically "visible" on every descendant too.
        if prop.name == policy::RESERVE_PROPERTY && prop.dataset == pool {
            match policy::parse_reserve_value(&prop.value) {
                Ok(r) => reserve = Some(r),
                Err(e) => eprintln!("cryo: {pool}: {e}"),
            }
        }
    }

    for (dataset, state) in datasets.iter_mut() {
        if state.limit.is_some()
            && let Some(&(referenced, used_by_snapshots)) = sizes.get(dataset)
        {
            state.referenced = referenced;
            state.used_by_snapshots = used_by_snapshots;
        }
    }

    Ok(reserve)
}

fn load_snapshots(datasets: &mut Datasets, pool: &str) -> Result<()> {
    let candidates: Vec<(zfs::SnapshotListing, snapname::ManagedSnapshot)> = zfs::list_snapshots(pool)?
        .into_iter()
        .filter_map(|listing| {
            let parsed = snapname::parse(&listing.full_name)?;
            Some((listing, parsed))
        })
        .collect();

    let names: Vec<String> = candidates
        .iter()
        .map(|(listing, _)| listing.full_name.clone())
        .collect();

    let claims: HashMap<String, String> = zfs::claimed_by_batch(&names)?.into_iter().collect();

    for (listing, parsed) in candidates {
        let claimed_by = claims
            .get(&listing.full_name)
            .map(|value| value.split(',').map(str::to_string).collect())
            .unwrap_or_default();

        datasets
            .entry(parsed.dataset)
            .or_default()
            .snapshots
            .push(ManagedSnapshotState {
                full_name: listing.full_name,
                instant: parsed.instant,
                // zfs's own creation time is authoritative; the name
                // only needs to parse to confirm it's cryo-managed.
                taken_at: Timestamp::from_epoch_secs(listing.creation_epoch_secs),
                claimed_by,
            });
    }

    Ok(())
}
