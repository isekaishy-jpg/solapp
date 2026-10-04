//! Optional configuration planning; applications retain Solworker runtimes.

use solworker::{SWRuntimeConfig, SWThreadPriority, SWWorkerConfig};

use crate::SACpuTopology;

/// Explicit inputs to independent physical-core class caps, in Low/Mid/High
/// order. There are no automatic reserved-core or class-count defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAWorkerRequest {
    /// Physical cores subtracted before the per-class ceiling is clamped.
    pub reserved_physical_cores: usize,
    /// Requested class limits. Zero disables that class; nonzero values are
    /// clamped independently between two and the computed ceiling.
    pub class_limits: [usize; 3],
    /// Optional OS priority request for each class, passed directly to SW.
    pub priorities: [Option<SWThreadPriority>; 3],
}

/// An explicit configuration calculation, not a started runtime or performance
/// recommendation. The application constructs SW and handles startup failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAWorkerPlan {
    requested: SAWorkerRequest,
    physical_cores: usize,
    class_ceiling: usize,
    effective: SWRuntimeConfig,
}

impl SAWorkerPlan {
    /// Applies the selected independent-class physical-core policy:
    /// `ceiling = clamp(physical cores - reserved cores, 2, 64)`; each nonzero
    /// request is clamped to `[2, ceiling]`, while zero remains disabled.
    ///
    /// Subtraction saturates before clamping. The aggregate SW budget equals
    /// the sum of effective class counts; it can exceed the number of physical
    /// or logical processors. This function installs no affinity, starts no
    /// workers and does not override application-chosen direct SW configuration.
    pub fn independent_physical_caps(topology: &SACpuTopology, request: SAWorkerRequest) -> Self {
        let physical_cores = topology.physical_cores();
        let class_ceiling = physical_cores
            .saturating_sub(request.reserved_physical_cores)
            .clamp(2, 64);
        let classes = std::array::from_fn(|index| {
            let requested = request.class_limits[index];
            let count = if requested == 0 {
                0
            } else {
                requested.clamp(2, class_ceiling)
            };
            let config = SWWorkerConfig::new(count);
            match request.priorities[index] {
                Some(priority) => config.with_priority(priority),
                None => config,
            }
        });
        // Three independently bounded counts have a sum at most 192.
        let budget = classes.iter().map(|class| class.worker_count()).sum();
        let effective = SWRuntimeConfig::new(budget, classes)
            .expect("bounded class sum is its aggregate budget");
        Self {
            requested: request,
            physical_cores,
            class_ceiling,
            effective,
        }
    }

    /// Unmodified application inputs, including priorities and disabled classes.
    pub fn requested(self) -> SAWorkerRequest {
        self.requested
    }
    /// Observed physical-core count used for this calculation.
    pub fn physical_cores(self) -> usize {
        self.physical_cores
    }
    /// Independent upper bound used for every nonzero class request.
    pub fn class_ceiling(self) -> usize {
        self.class_ceiling
    }
    /// Valid SW configuration; successful runtime construction is still required.
    pub fn effective(self) -> SWRuntimeConfig {
        self.effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SACoreAffinity, SACpuCore};
    use solworker::SWExecutionClass;

    fn topology(physical: usize, logical: usize) -> SACpuTopology {
        SACpuTopology {
            cores: (0..physical)
                .map(|index| SACpuCore {
                    affinities: vec![SACoreAffinity {
                        group: index as u16,
                        mask: 1,
                    }],
                    efficiency_class: 0,
                })
                .collect(),
            logical_processors: logical,
        }
    }
    fn request(limits: [usize; 3], reserved: usize) -> SAWorkerRequest {
        SAWorkerRequest {
            reserved_physical_cores: reserved,
            class_limits: limits,
            priorities: [
                Some(SWThreadPriority::BelowNormal),
                Some(SWThreadPriority::Normal),
                Some(SWThreadPriority::AboveNormal),
            ],
        }
    }
    fn counts(plan: SAWorkerPlan) -> [usize; 3] {
        SWExecutionClass::ALL.map(|class| plan.effective().workers_for(class).worker_count())
    }

    #[test]
    fn physical_input_and_independent_caps_do_not_become_an_aggregate_cpu_limit() {
        let plan = SAWorkerPlan::independent_physical_caps(&topology(6, 12), request([6; 3], 1));
        assert_eq!(counts(plan), [5; 3]);
        assert_eq!(plan.effective().worker_budget(), 15);
        let plan = SAWorkerPlan::independent_physical_caps(&topology(10, 12), request([6; 3], 1));
        assert_eq!(counts(plan), [6; 3]);
        assert_eq!(plan.effective().worker_budget(), 18);
        assert_eq!(plan.requested(), request([6; 3], 1));
        for (class, priority) in SWExecutionClass::ALL
            .into_iter()
            .zip(plan.requested().priorities)
        {
            assert_eq!(
                plan.effective().workers_for(class).requested_priority(),
                priority
            );
        }
    }

    #[test]
    fn zero_is_disabled_and_floor_ceiling_arithmetic_is_bounded() {
        let plan = SAWorkerPlan::independent_physical_caps(
            &topology(1, 2),
            request([0, 1, usize::MAX], usize::MAX),
        );
        assert_eq!(counts(plan), [0, 2, 2]);
        let plan = SAWorkerPlan::independent_physical_caps(
            &topology(128, 256),
            request([usize::MAX; 3], 0),
        );
        assert_eq!(counts(plan), [64; 3]);
        assert_eq!(plan.effective().worker_budget(), 192);
        let plan = SAWorkerPlan::independent_physical_caps(&topology(8, 16), request([0; 3], 0));
        assert_eq!(counts(plan), [0; 3]);
        assert_eq!(plan.effective().worker_budget(), 0);
    }
}
