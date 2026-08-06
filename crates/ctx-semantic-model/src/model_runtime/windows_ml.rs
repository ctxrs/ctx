use anyhow::Result;

use super::onnx::LoadedOnnxRuntime;

#[derive(Default)]
pub(super) struct WindowsMlProviderRegistration {
    identity: String,
    #[cfg(target_os = "windows")]
    libraries: Vec<ort::ep::ExecutionProviderLibrary>,
}

impl WindowsMlProviderRegistration {
    pub(super) fn identity(&self) -> &str {
        if self.identity.is_empty() {
            "included-only"
        } else {
            &self.identity
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsMlProviderRegistration {
    fn drop(&mut self) {
        for library in self.libraries.drain(..).rev() {
            let _ = library.unregister();
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{
        ffi::{c_char, c_void, CStr},
        path::{Path, PathBuf},
        ptr,
        sync::{Mutex, OnceLock},
    };

    use anyhow::{anyhow, Context, Result};
    use libloading::os::windows::{
        Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };

    use super::{LoadedOnnxRuntime, WindowsMlProviderRegistration};

    const WINDOWS_ML_API_DLL: &str = "Microsoft.Windows.AI.MachineLearning.dll";
    const READY: i32 = 0;
    const NOT_READY: i32 = 1;
    const NOT_PRESENT: i32 = 2;
    const CERTIFIED: i32 = 1;

    type CatalogHandle = *mut c_void;
    type ProviderHandle = *mut c_void;
    type HResult = i32;
    type Bool = i32;

    #[repr(C)]
    struct ProviderInfo {
        name: *const c_char,
        version: *const c_char,
        package_family_name: *const c_char,
        library_path: *const c_char,
        package_root_path: *const c_char,
        ready_state: i32,
        certification: i32,
    }

    type CatalogCreate = unsafe extern "system" fn(*mut CatalogHandle) -> HResult;
    type CatalogRelease = unsafe extern "system" fn(CatalogHandle);
    type EnumCallback =
        unsafe extern "system" fn(ProviderHandle, *const ProviderInfo, *mut c_void) -> Bool;
    type CatalogEnum =
        unsafe extern "system" fn(CatalogHandle, EnumCallback, *mut c_void) -> HResult;
    type EnsureReady = unsafe extern "system" fn(ProviderHandle) -> HResult;
    type GetLibraryPathSize = unsafe extern "system" fn(ProviderHandle, *mut usize) -> HResult;
    type GetLibraryPath =
        unsafe extern "system" fn(ProviderHandle, usize, *mut c_char, *mut usize) -> HResult;

    struct WindowsMlApi {
        library: libloading::Library,
        path: PathBuf,
    }

    #[derive(Debug)]
    struct Provider {
        handle: ProviderHandle,
        name: String,
        version: String,
        ready_state: i32,
        certification: i32,
    }

    static API: OnceLock<WindowsMlApi> = OnceLock::new();
    static API_LOCK: Mutex<()> = Mutex::new(());

    fn succeeded(result: HResult) -> bool {
        result >= 0
    }

    unsafe extern "system" fn collect_provider(
        handle: ProviderHandle,
        info: *const ProviderInfo,
        context: *mut c_void,
    ) -> Bool {
        if info.is_null() || context.is_null() {
            return 1;
        }
        let info = &*info;
        if info.name.is_null() {
            return 1;
        }
        let providers = &mut *(context.cast::<Vec<Provider>>());
        let name = CStr::from_ptr(info.name).to_string_lossy().into_owned();
        let version = if info.version.is_null() {
            String::new()
        } else {
            CStr::from_ptr(info.version).to_string_lossy().into_owned()
        };
        providers.push(Provider {
            handle,
            name,
            version,
            ready_state: info.ready_state,
            certification: info.certification,
        });
        1
    }

    impl WindowsMlApi {
        fn load(runtime: &LoadedOnnxRuntime) -> Result<&'static Self> {
            if let Some(api) = API.get() {
                return api.matches(runtime);
            }
            let _guard = API_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(api) = API.get() {
                return api.matches(runtime);
            }
            let root = runtime_root(&runtime.path)?;
            let path = root.join("lib").join(WINDOWS_ML_API_DLL);
            let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
            let library = unsafe { WindowsLibrary::load_with_flags(&path, flags) }
                .with_context(|| format!("load verified Windows ML runtime {}", path.display()))?;
            let api = WindowsMlApi {
                library: library.into(),
                path,
            };
            let _ = API.set(api);
            API.get()
                .ok_or_else(|| anyhow!("Windows ML API initialization failed"))?
                .matches(runtime)
        }

        fn matches(&self, runtime: &LoadedOnnxRuntime) -> Result<&Self> {
            let expected = runtime_root(&runtime.path)?
                .join("lib")
                .join(WINDOWS_ML_API_DLL);
            if self.path != expected {
                return Err(anyhow!(
                    "Windows ML was already initialized from a different verified runtime"
                ));
            }
            Ok(self)
        }

        unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> Result<T> {
            self.library
                .get::<T>(name)
                .map(|symbol| *symbol)
                .with_context(|| {
                    format!(
                        "load Windows ML API symbol {}",
                        String::from_utf8_lossy(name).trim_end_matches('\0')
                    )
                })
        }

        fn with_catalog<T>(
            &self,
            operation: impl FnOnce(&Self, CatalogHandle, Vec<Provider>) -> Result<T>,
        ) -> Result<T> {
            unsafe {
                let create: CatalogCreate = self.symbol(b"WinMLEpCatalogCreate\0")?;
                let release: CatalogRelease = self.symbol(b"WinMLEpCatalogRelease\0")?;
                let enumerate: CatalogEnum = self.symbol(b"WinMLEpCatalogEnumProviders\0")?;
                let mut catalog = ptr::null_mut();
                let result = create(&mut catalog);
                if !succeeded(result) || catalog.is_null() {
                    return Err(anyhow!(
                        "Windows ML execution-provider catalog is unavailable (HRESULT 0x{:08x})",
                        result as u32
                    ));
                }
                let mut providers = Vec::new();
                let enum_result = enumerate(
                    catalog,
                    collect_provider,
                    (&mut providers as *mut Vec<Provider>).cast(),
                );
                if !succeeded(enum_result) {
                    release(catalog);
                    return Err(anyhow!(
                        "Windows ML execution-provider discovery failed (HRESULT 0x{:08x})",
                        enum_result as u32
                    ));
                }
                let outcome = operation(self, catalog, providers);
                release(catalog);
                outcome
            }
        }

        fn ensure_ready(&self, provider: ProviderHandle) -> Result<()> {
            unsafe {
                let ensure: EnsureReady = self.symbol(b"WinMLEpEnsureReady\0")?;
                let result = ensure(provider);
                if !succeeded(result) {
                    return Err(anyhow!(
                        "Windows ML execution-provider preparation failed (HRESULT 0x{:08x})",
                        result as u32
                    ));
                }
                Ok(())
            }
        }

        fn library_path(&self, provider: ProviderHandle) -> Result<PathBuf> {
            unsafe {
                let get_size: GetLibraryPathSize = self.symbol(b"WinMLEpGetLibraryPathSize\0")?;
                let get_path: GetLibraryPath = self.symbol(b"WinMLEpGetLibraryPath\0")?;
                let mut size = 0;
                let result = get_size(provider, &mut size);
                if !succeeded(result) || size < 2 || size > 32 * 1024 {
                    return Err(anyhow!(
                        "Windows ML execution provider returned an invalid library path"
                    ));
                }
                let mut bytes = vec![0_u8; size];
                let mut used = 0;
                let result = get_path(
                    provider,
                    bytes.len(),
                    bytes.as_mut_ptr().cast::<c_char>(),
                    &mut used,
                );
                if !succeeded(result) || used == 0 || used > bytes.len() {
                    return Err(anyhow!(
                        "Windows ML execution provider library path is unavailable"
                    ));
                }
                bytes.truncate(used.saturating_sub(1));
                let path = PathBuf::from(
                    String::from_utf8(bytes)
                        .context("Windows ML execution provider path is not UTF-8")?,
                );
                if !path.is_absolute() {
                    return Err(anyhow!(
                        "Windows ML execution provider returned a relative library path"
                    ));
                }
                Ok(path)
            }
        }
    }

    fn runtime_root(path: &Path) -> Result<&Path> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("Windows ML runtime library has no parent"))?;
        if parent.file_name().and_then(|name| name.to_str()) == Some("lib") {
            parent
                .parent()
                .ok_or_else(|| anyhow!("Windows ML runtime lib directory has no parent"))
        } else {
            Ok(parent)
        }
    }

    fn provider_identity(providers: &[Provider]) -> String {
        let mut identities = providers
            .iter()
            .filter(|provider| provider.certification == CERTIFIED)
            .map(|provider| {
                if provider.version.is_empty() {
                    provider.name.clone()
                } else {
                    format!("{}@{}", provider.name, provider.version)
                }
            })
            .collect::<Vec<_>>();
        identities.sort();
        if identities.is_empty() {
            "included-only".to_owned()
        } else {
            identities.join(",")
        }
    }

    pub(super) fn provision_catalog(runtime: &LoadedOnnxRuntime) -> Result<String> {
        let api = WindowsMlApi::load(runtime)?;
        api.with_catalog(|api, _catalog, providers| {
            let mut prepared = Vec::new();
            for provider in providers
                .iter()
                .filter(|provider| provider.certification == CERTIFIED)
            {
                if matches!(provider.ready_state, NOT_PRESENT | NOT_READY) {
                    api.ensure_ready(provider.handle)
                        .with_context(|| format!("prepare certified provider {}", provider.name))?;
                }
                prepared.push(Provider {
                    handle: provider.handle,
                    name: provider.name.clone(),
                    version: provider.version.clone(),
                    ready_state: READY,
                    certification: provider.certification,
                });
            }
            Ok(provider_identity(&prepared))
        })
    }

    pub(super) fn register_ready_providers(
        runtime: &LoadedOnnxRuntime,
    ) -> Result<WindowsMlProviderRegistration> {
        let api = WindowsMlApi::load(runtime)?;
        api.with_catalog(|api, _catalog, providers| {
            let environment = ort::environment::Environment::current()
                .context("open Windows ML ONNX Runtime environment")?;
            let mut registration = WindowsMlProviderRegistration::default();
            let mut registered = Vec::new();
            for provider in providers
                .iter()
                .filter(|provider| provider.certification == CERTIFIED)
            {
                if provider.ready_state == NOT_PRESENT {
                    continue;
                }
                if provider.ready_state == NOT_READY {
                    api.ensure_ready(provider.handle).with_context(|| {
                        format!("activate installed certified provider {}", provider.name)
                    })?;
                }
                let path = api.library_path(provider.handle)?;
                let library = environment
                    .register_ep_library(provider.name.clone(), &path)
                    .with_context(|| {
                        format!("register certified Windows ML provider {}", provider.name)
                    })?;
                registration.libraries.push(library);
                registered.push(Provider {
                    handle: provider.handle,
                    name: provider.name.clone(),
                    version: provider.version.clone(),
                    ready_state: READY,
                    certification: provider.certification,
                });
            }
            registration.identity = provider_identity(&registered);
            Ok(registration)
        })
    }
}

#[cfg(target_os = "windows")]
pub(super) fn provision_catalog(runtime: &LoadedOnnxRuntime) -> Result<String> {
    platform::provision_catalog(runtime)
}

#[cfg(target_os = "windows")]
pub(super) fn register_ready_providers(
    runtime: &LoadedOnnxRuntime,
) -> Result<WindowsMlProviderRegistration> {
    platform::register_ready_providers(runtime)
}

#[cfg(not(target_os = "windows"))]
pub(super) fn register_ready_providers(
    _runtime: &LoadedOnnxRuntime,
) -> Result<WindowsMlProviderRegistration> {
    Ok(WindowsMlProviderRegistration::default())
}

#[cfg(target_os = "windows")]
pub(super) fn runtime_is_windows_ml(runtime: &LoadedOnnxRuntime) -> bool {
    runtime
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("onnxruntime.dll"))
        && std::path::Path::new(&runtime.path)
            .parent()
            .and_then(std::path::Path::parent)
            .is_some_and(|root| {
                root.join("lib")
                    .join("Microsoft.Windows.AI.MachineLearning.dll")
                    .is_file()
            })
}
