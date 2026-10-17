//! Edge legacy / IE 10+ (Autopsy `ExtractEdge`): `WebCacheV01.dat` under
//! a `WebCache` folder and `Spartan.edb` under `MicrosoftEdge`.

use pm_ingest::{JobContext, ModuleError};

use super::{Browser, Web};

#[allow(
    clippy::unnecessary_wraps,
    reason = "same signature as the other web steps"
)]
pub(crate) fn extract(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    let webcache = web.named("WebCacheV01.dat", "WebCache");
    let spartan = web.named("Spartan.edb", "MicrosoftEdge");
    if webcache.is_empty() && spartan.is_empty() {
        return Ok(());
    }
    web.set_found(Browser::Edge);
    let _ = ctx;
    Ok(())
}
