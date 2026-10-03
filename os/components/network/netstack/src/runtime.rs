//! 生命周期骨架；可打包不代表已提供网络服务。

use kcomp_sdk::errno::Errno;

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    // TODO: 定稿 create config，绑定组合者选定的 NetDevice；
    // 构造私有 SocketPool、DeviceStack / 帧 staging、NetworkInstance / Engine、Clock；
    // 实例私有 backing 由 runtime 提供，失败逐项回滚。
    // NetworkService<NetworkInstance> 发布一个服务 endpoint，worker 另持 DevicePort。
    // 服务只做有界操作，worker 推进协议；两者经 Engine 串行访问，外调在解锁后。
    // SDK C / Gate adapters、Core timer / 跨组件通知实现后才发布 endpoint / start task。
    kcomp_sdk::klog!("netstack: skeleton; network service not implemented");
    Errno::ENOTSUP.code()
});

kcomp_sdk::kcomp_instance_destroy!(|_state| {
    // TODO: 拒绝新操作、撤销通知和 timer、排空 worker / 在途调用，退休 socket。
    // create 尚未成功，正常生命周期不会进入此处。
    Errno::ENOTSUP.code()
});
