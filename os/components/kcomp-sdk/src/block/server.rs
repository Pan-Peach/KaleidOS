//! Owned Block Server Task; the schema generates its business decoder.
use crate::{Errno, Result, block::BlockDeviceProvider, generated::block_wire, ipc};
pub fn serve<P: BlockDeviceProvider>(provider: &P, endpoint: u64) -> Result<()> {
    ipc::listen(endpoint)?;
    let mut bytes = [0; ipc::MESSAGE_MAX];
    loop {
        let (receipt, _, _, len) = match ipc::receive(endpoint, &mut bytes) {
            Ok(message) => message,
            Err(Errno::EAGAIN) => {
                ipc::wait_receive(endpoint)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let request = match ipc::service::Request::decode(&bytes[..len]) {
            Ok(request) => request,
            Err(error) => {
                let _ = ipc::reply(receipt, &error.code().to_le_bytes());
                continue;
            }
        };
        let mut reply = [0; ipc::MESSAGE_MAX];
        let output = &mut reply[4..4 + request.output];
        let status = block_wire::dispatch(provider, &request, output);
        // Cancellation does not undo a write already executed by the device.
        let _ = ipc::service::reply(receipt, status, &mut reply[..4 + request.output]);
    }
}
