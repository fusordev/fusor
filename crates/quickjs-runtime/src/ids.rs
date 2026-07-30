use crate::arena::Id;

pub(crate) enum RealmMarker {}
pub(crate) enum InstalledCodeMarker {}
pub(crate) enum FunctionMarker {}
pub(crate) enum BindingCellMarker {}

pub(crate) type RealmId = Id<RealmMarker>;
pub(crate) type InstalledCodeId = Id<InstalledCodeMarker>;
pub(crate) type FunctionId = Id<FunctionMarker>;
pub(crate) type BindingCellId = Id<BindingCellMarker>;
