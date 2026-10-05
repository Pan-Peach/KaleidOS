# Source inventory, not the runtime component graph. Names come from basenames.
PRODUCTION_RUST := scheduler_rr drivers/virtio_blk driver_prober filesystems/vfs \
                   personalities/posix network/netstack init ksh kbench
PRODUCTION_C := filesystems/fatfs filesystems/littlefs
HOST_FIXTURE_RUST := tests/core_test tests/kcomp_smoke tests/kcomp_heap \
                     tests/kcomp_domain_service tests/kcomp_min tests/kcomp_isolated \
                     tests/kcomp_isolated_life tests/kcomp_isolated_svc tests/kcomp_isolated_bad \
                     tests/kcomp_isolated_direct tests/kcomp_isolated_unsupported
GUEST_FIXTURE_RUST := tests/kcomp_smp tests/kcomp_panic tests/drivers/ram_blk tests/drivers/ram_blk_rw
# kcomp_min pins relocation layout for host tests; it is not a guest scenario.
TEST_RUST := $(filter-out tests/kcomp_min,$(HOST_FIXTURE_RUST)) $(GUEST_FIXTURE_RUST)
TEST_C := tests/kcomp_c_smoke
KCOMP_SRCS := $(PRODUCTION_RUST) $(if $(filter y,$(CONFIG_TEST_COMPONENTS)),$(TEST_RUST),)
KCOMP_C_SRCS := $(PRODUCTION_C) $(if $(filter y,$(CONFIG_TEST_COMPONENTS)),$(TEST_C),)

# One development inventory, reused by fmt, lint and host testing.
ALL_RUST_CRATES := os/core os/arch os/components/kcomp-sdk \
                   $(addprefix os/components/,$(PRODUCTION_RUST) $(HOST_FIXTURE_RUST) $(GUEST_FIXTURE_RUST)) \
                   os/boot/riscv os/boot/x86_64 os/boot/aarch64 os/boot/loongarch64
HOST_CRATES := os/components/kcomp-sdk os/components/driver_prober os/components/init \
               os/components/ksh os/components/personalities/posix os/components/kbench \
               os/components/tests/drivers/ram_blk
TARGET_CRATES := $(filter-out os/core os/arch os/components/kcomp-sdk os/boot/%,$(ALL_RUST_CRATES))
