//! 生命周期占位；load posix 当前必须失败，不能宣称用户程序环境已就绪。

use kcomp_sdk::errno::Errno;

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    // TODO: 校验 config_abi / 长度，复制 PosixCreateConfig，验证 VFS endpoint；
    // bind 后构造 per-instance process / thread / fd 状态，失败回滚。
    // 用户 trap 路由和执行域机制就绪前，不创建假进程或发布 endpoint。
    kcomp_sdk::klog!("posix: skeleton; userspace not implemented");
    Errno::ENOTSUP.code()
});

kcomp_sdk::kcomp_instance_destroy!(|_state| {
    // TODO: 停止进程 / 用户任务，释放 fd 引用，完成协作式逻辑退役。
    // create 不成功，正常生命周期不会进入此处。
    Errno::ENOTSUP.code()
});
