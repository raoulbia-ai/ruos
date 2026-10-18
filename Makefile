# ruOS build. Outputs go to build/.
#   make            kernel + userland + initramfs
#   make run        boot in QEMU (CONFIG=file passes KEY=VALUE settings, e.g. the API key)
#   make test       userland unit tests + scripted boot test in QEMU

include kernel/VERSION

BUILD     := $(CURDIR)/build
KSRC      := $(BUILD)/linux-$(LINUX_VERSION)
BZIMAGE   := $(BUILD)/bzImage
INITRAMFS := $(BUILD)/initramfs.cpio.gz
TARGET    := x86_64-unknown-linux-musl
BINDIR    := userland/target/$(TARGET)/release
BUSYBOX   ?= /bin/busybox
JOBS      ?= $(shell nproc)
CONFIG    ?=

.PHONY: all kernel userland initramfs run test unit-test boot-test clean

all: kernel initramfs

kernel: $(BZIMAGE)

$(BUILD)/linux-$(LINUX_VERSION).tar.xz:
	mkdir -p $(BUILD)
	curl -fsSL -o $@.part https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$(LINUX_VERSION).tar.xz
	echo "$(LINUX_SHA256)  $@.part" | sha256sum -c -
	mv $@.part $@

$(KSRC)/.config: $(BUILD)/linux-$(LINUX_VERSION).tar.xz kernel/ruos.config
	rm -rf $(KSRC) && tar -C $(BUILD) -xf $<
	$(MAKE) -C $(KSRC) x86_64_defconfig
	cd $(KSRC) && scripts/kconfig/merge_config.sh -m .config $(CURDIR)/kernel/ruos.config
	$(MAKE) -C $(KSRC) olddefconfig

$(BZIMAGE): $(KSRC)/.config
	$(MAKE) -C $(KSRC) -j$(JOBS) bzImage
	cp $(KSRC)/arch/x86/boot/bzImage $@

userland:
	cd userland && cargo build --release --target $(TARGET)

initramfs: userland
	mkdir -p $(BUILD)
	tools/mkinitramfs.sh $(INITRAMFS) $(BINDIR) $(BUSYBOX)

run: all
	tools/run-qemu.sh $(BZIMAGE) $(INITRAMFS) $(CONFIG)

unit-test:
	cd userland && cargo test

boot-test: all
	tools/boot-test.py $(BZIMAGE) $(INITRAMFS)

test: unit-test boot-test

clean:
	rm -rf $(BUILD)
	cd userland && cargo clean
