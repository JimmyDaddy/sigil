use sigil_kernel::TaskConfig;

use super::{
    configured_max_parallel_changeset_steps, configured_max_parallel_read_steps,
    configured_provider_route_concurrency_limit,
};

#[test]
fn task_role_parallelism_has_nonzero_independent_bounds_and_a_shared_route_ceiling() {
    let mut config = TaskConfig {
        max_parallel_read_steps: 4,
        max_parallel_changeset_steps: 2,
        ..TaskConfig::default()
    };
    assert_eq!(configured_max_parallel_read_steps(&config), 4);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 2);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 4);

    config.max_parallel_read_steps = 2;
    config.max_parallel_changeset_steps = 3;
    assert_eq!(configured_provider_route_concurrency_limit(&config), 3);

    config.max_parallel_read_steps = 0;
    config.max_parallel_changeset_steps = 0;
    assert_eq!(configured_max_parallel_read_steps(&config), 1);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 1);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 1);
}
