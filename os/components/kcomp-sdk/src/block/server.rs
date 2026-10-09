//! Owned Block Server Task; the existing business decoder also serves IPC.
use crate::{Errno, Result, block::BlockDeviceProvider, frame::Call, ipc};
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
        // One sector per wire operation. SDK clients split larger transfers.
        let status = if request.input.len() > 512 || request.output > 512 {
            Errno::EMSGSIZE.code()
        } else {
            super::dispatch::dispatch(
                provider,
                request.method,
                Call {
                    args: request.args,
                    input: request.input,
                    output,
                },
            )
        };
        // Cancellation does not undo a write already executed by the device.
        let _ = ipc::service::reply(receipt, status, &mut reply[..4 + request.output]);
    }
}
