use super::*;

#[derive(Clone)]
pub struct SourceOwner {
    pub route_index: usize,
    pub source: SourceKey,
    pub present: bool,
    pub revalidation: Option<SourceBackedRouteRevalidation>,
}

impl SourceOwner {
    pub fn new(
        route_index: usize,
        source: SourceKey,
        present: bool,
        revalidation: Option<SourceBackedRouteRevalidation>,
    ) -> Self {
        Self {
            route_index,
            source,
            present,
            revalidation,
        }
    }

    pub fn route_index(&self) -> usize {
        self.route_index
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn is_present(&self) -> bool {
        self.present
    }

    pub fn revalidation(&self) -> Option<&SourceBackedRouteRevalidation> {
        self.revalidation.as_ref()
    }
}

#[derive(Clone)]
pub enum SourceBackedRouteRevalidation {
    Source(CertifiedSource),
    Deletion(Box<CertifiedSourceDeletion>),
}

#[derive(Clone)]
pub struct CompleteInventoryOwner {
    pub route_index: usize,
    pub inventory: CertifiedSourceInventory,
}

impl CompleteInventoryOwner {
    pub fn new(route_index: usize, inventory: CertifiedSourceInventory) -> Self {
        Self {
            route_index,
            inventory,
        }
    }

    pub fn route_index(&self) -> usize {
        self.route_index
    }

    pub fn inventory(&self) -> &CertifiedSourceInventory {
        &self.inventory
    }
}
