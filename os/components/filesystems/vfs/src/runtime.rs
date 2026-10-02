//! 组件生命周期占位。可构建镜像，不表示 VFS 服务已经可用。

use kcomp_sdk::errno::Errno;

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    // TODO: 解析组合配置，构造 per-instance state，bind FS providers；
    // 服务 ABI 定稿并实现后才 staged publish。构造失败须自行清理。
    kcomp_sdk::klog!("vfs: skeleton; service not implemented");
    Errno::ENOTSUP.code()
});

kcomp_sdk::kcomp_instance_destroy!(|_state| {
    // TODO: 停止新请求，处理打开引用与在途 I/O，完成协作式 teardown。
    // create 尚不成功，正常生命周期不会进入本入口。
    Errno::ENOTSUP.code()
});
