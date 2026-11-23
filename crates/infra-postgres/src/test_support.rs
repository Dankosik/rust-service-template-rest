use std::time::Duration;

use sqlx::PgPool;

use crate::{Pool, PoolStartup};

pub const PASSWORD_REFRESH_INTERVAL: Duration = crate::credentials::PASSWORD_REFRESH_INTERVAL;

#[must_use]
pub fn pool_from_sqlx(inner: PgPool) -> Pool {
    Pool::from_test_pool(inner)
}

pub fn connect_lazy(url: &str) -> Result<Pool, sqlx::Error> {
    Pool::connect_lazy_for_test(url)
}

pub fn startup_from_pool(pool: Pool) -> PoolStartup {
    PoolStartup::from_test_pool(pool)
}

#[must_use]
pub fn as_sqlx(pool: &Pool) -> &PgPool {
    pool.as_sqlx()
}
