//! 只做类型检查的调用方用例，不执行 todo!()，不作为网络功能测试。
//! 三个场景只依赖 SDK；Wait 是示例调用方的调度策略，不进入服务契约。
#![allow(dead_code)]

use super::*;
use crate::endpoint::Endpoint;
use crate::generated::network::KCOMP_NETWORK_IO_MAX;

trait Wait {
    fn task(&self) -> u32;
    fn park(&mut self) -> NetworkResult<()>;
    fn yield_now(&mut self) -> NetworkResult<()>;
}

fn retry_busy<T>(
    wait: &mut impl Wait,
    mut op: impl FnMut() -> NetworkResult<T>,
) -> NetworkResult<T> {
    loop {
        match op() {
            Err(NetworkError::Busy | NetworkError::Transport(Errno::EBUSY)) => wait.yield_now()?,
            result => return result,
        }
    }
}

fn wait_ready<T>(
    socket: &impl InetSocket,
    wait: &mut impl Wait,
    mut op: impl FnMut() -> NetworkResult<Attempt<T>>,
) -> NetworkResult<T> {
    let task = wait.task();
    let mut subscription = retry_busy(wait, || socket.subscribe(task))?;
    let result = (|| loop {
        // 登记之后重试：覆盖数据早于订阅到达，以及检查与 park 之间的通知。
        match retry_busy(wait, &mut op)? {
            Attempt::Ready(value) => return Ok(value),
            Attempt::Pending => wait.park()?,
        }
    })();
    let cancelled = retry_busy(wait, || subscription.cancel());
    result.and_then(|value| cancelled.map(|()| value))
}

fn send_all(tcp: &TcpSocket<'_>, wait: &mut impl Wait, data: &[u8]) -> NetworkResult<()> {
    let mut sent = 0;
    while sent < data.len() {
        let end = sent.saturating_add(KCOMP_NETWORK_IO_MAX).min(data.len());
        let count = wait_ready(tcp, wait, || tcp.try_send(&data[sent..end]))?;
        if count == 0 || count > end - sent {
            return Err(NetworkError::InvalidReply);
        }
        sent += count;
    }
    Ok(())
}

/// 发起连接、发送完整请求、结束发送方向，再接收一个响应片段。
fn tcp_client(
    endpoint: Endpoint<Network>,
    family: AddressFamily,
    peer: SocketAddress,
    request: &[u8],
    response: &mut [u8],
    wait: &mut impl Wait,
) -> NetworkResult<StreamReceive> {
    let net = retry_busy(wait, || endpoint.bind())?;
    let mut tcp = retry_busy(wait, || net.tcp_socket(family))?;
    let result = (|| {
        retry_busy(wait, || tcp.start_connect(peer))?;
        wait_ready(&tcp, wait, || {
            let status = tcp.status()?;
            if let Some(errno) = status.failure {
                return Err(NetworkError::Method(errno));
            }
            match status.phase {
                ConnectionPhase::Established => Ok(Attempt::Ready(())),
                ConnectionPhase::Connecting => Ok(Attempt::Pending),
                _ => Err(NetworkError::Method(Errno::EINVAL.code())),
            }
        })?;
        send_all(&tcp, wait, request)?;
        retry_busy(wait, || tcp.finish_send())?;
        wait_ready(&tcp, wait, || tcp.try_receive(response))
    })();
    let closed = retry_busy(wait, || tcp.close());
    result.and_then(|value| closed.map(|()| value))
}

/// 先 bind 再 listen；accept 后关监听器，再用已接管连接回送一个片段。
fn tcp_server(
    endpoint: Endpoint<Network>,
    family: AddressFamily,
    local: BindAddress,
    buffer: &mut [u8],
    wait: &mut impl Wait,
) -> NetworkResult<()> {
    let net = retry_busy(wait, || endpoint.bind())?;
    let mut listener = retry_busy(wait, || net.tcp_socket(family))?;
    let accepted = (|| {
        retry_busy(wait, || listener.bind_local(local))?;
        retry_busy(wait, || listener.listen(8))?;
        wait_ready(&listener, wait, || listener.try_accept())
    })();
    // 接管出的代理借用 net，不能借用 listener，否则这里无法关闭监听器。
    let listener_closed = retry_busy(wait, || listener.close());
    let mut connection = accepted?;
    let result = listener_closed.and_then(|()| {
        match wait_ready(&connection, wait, || connection.try_receive(buffer))? {
            StreamReceive::Bytes(count) => send_all(&connection, wait, &buffer[..count])?,
            StreamReceive::End => {}
        }
        retry_busy(wait, || connection.finish_send())
    });
    let closed = retry_busy(wait, || connection.close());
    result.and(closed)
}

/// UDP 查询：自动分配源端口，只接收目标 peer 的后续包，保留短缓冲 / 零长度包信息。
fn udp_query(
    endpoint: Endpoint<Network>,
    family: AddressFamily,
    peer: SocketAddress,
    request: &[u8],
    response: &mut [u8],
    wait: &mut impl Wait,
) -> NetworkResult<ReceivedDatagram> {
    let net = retry_busy(wait, || endpoint.bind())?;
    let mut udp = retry_busy(wait, || net.udp_socket(family))?;
    let result = (|| {
        retry_busy(wait, || udp.set_receive_peer(Some(peer)))?;
        wait_ready(&udp, wait, || udp.try_send_to(peer, request))?;
        wait_ready(&udp, wait, || {
            udp.try_receive(response, DatagramReadMode::Whole)
        })
    })();
    let closed = retry_busy(wait, || udp.close());
    result.and_then(|value| closed.map(|()| value))
}
