//! Hidden mutation seams for external qualification targets.

use super::*;

impl SourceBackedRoute {
    #[doc(hidden)]
    pub fn metadata_for_test_mut(&mut self) -> &mut SourceBackedRouteMetadata {
        &mut self.metadata
    }

    #[doc(hidden)]
    pub fn registration_sources_for_test_mut(&mut self) -> &mut [ProviderSource] {
        &mut self.registration_sources
    }

    #[doc(hidden)]
    pub fn driver_for_test(&self) -> Option<&SourceBackedRouteDriver> {
        self.driver.as_ref()
    }

    #[doc(hidden)]
    pub fn take_driver_for_test(&mut self) -> Option<SourceBackedRouteDriver> {
        self.driver.take()
    }

    #[doc(hidden)]
    pub fn set_driver_for_test(&mut self, driver: Option<SourceBackedRouteDriver>) {
        self.driver = driver;
    }
}
