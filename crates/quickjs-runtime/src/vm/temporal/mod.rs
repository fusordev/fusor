//! Initial `Temporal.Instant` JavaScript boundary over `temporal_rs`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

mod common;
mod duration;
mod instant;
mod now;
mod plain_date;
mod plain_date_time;
mod plain_month_day;
mod plain_time;
mod plain_year_month;
mod zoned_date_time;

#[allow(
    clippy::wildcard_imports,
    reason = "private VM sibling modules share one interpreter implementation namespace"
)]
pub(super) use {
    common::*, duration::*, instant::*, now::*, plain_date::*, plain_date_time::*,
    plain_month_day::*, plain_time::*, plain_year_month::*, zoned_date_time::*,
};
