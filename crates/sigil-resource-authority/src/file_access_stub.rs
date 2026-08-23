//! Contract-only file access stub for factory composition tests.

use std::sync::Arc;

use sigil_kernel::managed_file_access::{
    ManagedFileAccessAdmissionTokenV1, ManagedFileAccessRequestV1, ManagedFileAccessResultV1,
    ManagedFileAccessServiceV1,
};

#[derive(Debug)]
pub struct StubFileAccessServiceV1;

impl Default for StubFileAccessServiceV1 {
    fn default() -> Self {
        Self
    }
}

impl StubFileAccessServiceV1 {
    pub fn arc() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl ManagedFileAccessServiceV1 for StubFileAccessServiceV1 {
    fn access(
        &self,
        _request: ManagedFileAccessRequestV1,
        _token: ManagedFileAccessAdmissionTokenV1,
    ) -> Result<
        ManagedFileAccessResultV1,
        sigil_kernel::managed_file_access::ManagedFileAccessErrorV1,
    > {
        Err(sigil_kernel::managed_file_access::ManagedFileAccessErrorV1::OperationNotPermitted)
    }
}

pub fn stub_file_access_service() -> Arc<dyn ManagedFileAccessServiceV1> {
    StubFileAccessServiceV1::arc()
}
