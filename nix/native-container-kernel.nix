# Fixed native-KVM execution, never a fallback from a failed namespace build.
{
  pkgs,
  system,
  program,
}:
let
  inherit (pkgs.lib) escapeShellArg;
  storeProjection = pkgs.pkgsStatic.stdenv.mkDerivation {
    pname = "loom-native-store-projection";
    version = "1";
    src = ./native-store-projection.c;
    dontUnpack = true;
    nativeBuildInputs = [ pkgs.buildPackages.binutils ];
    buildPhase = ''
      $CC -std=c11 -Os -Wall -Wextra -Werror "$src" -o project-store
    '';
    doCheck = true;
    checkPhase = ''
      readelf -l project-store > program-headers
      if grep -F INTERP program-headers; then
        echo 'input projector must not need an interpreter before projection' >&2
        exit 1
      fi
      code=0
      ./project-store || code=$?
      [[ "$code" == 64 ]]
      code=0
      ./project-store /missing-command || code=$?
      [[ "$code" == 78 ]]
    '';
    installPhase = ''install -Dm755 project-store "$out/bin/project-store"'';
  };
  kernelConfig = pkgs.runCommand "linux-generate-config-immutable-shell" { } ''
    substitute ${pkgs.path + "/pkgs/os-specific/linux/kernel/generate-config.pl"} "$out" \
      --replace-fail 'my $pid = open2(\*IN, \*OUT,' \
        'my $pid = open2(\*IN, \*OUT, "${pkgs.bash}/bin/bash", "-c",'
  '';
  kernelModules = pkgs.writeText "native-kernel-modules" (
    "Module Size Used by\n"
    + pkgs.lib.concatMapStrings (name: "${name} 0 0\n") [
      "9p"
      "9pnet"
      "9pnet_virtio"
      "virtio_pci"
      "virtio_pci_modern_dev"
      "virtio_pci_legacy_dev"
      "virtio_ring"
      "virtio"
      "virtio_blk"
      "virtio_rng"
      "fuse"
      "tun"
      "overlay"
      "veth"
      "bridge"
      "br_netfilter"
      "loop"
      "configfs"
      "autofs4"
      "efivarfs"
      "ext2"
      "ext4"
      "tpm_crb"
      "tpm_tis"
      "tpm_tis_core"
      "tpm"
      "unix"
      "ipv6"
      "af_packet"
      "dm_mod"
      "virtio_console"
      "binfmt_misc"
      "nf_tables"
      "nft_compat"
      "nft_chain_nat"
      "nft_ct"
      "nft_counter"
      "nft_masq"
      "nft_redir"
      "nft_reject"
      "nft_reject_ipv4"
      "nft_reject_ipv6"
      "nft_log"
      "nft_fib"
      "nft_fib_inet"
      "nft_fib_ipv4"
      "nft_fib_ipv6"
      "nft_limit"
      "nft_numgen"
      "nft_quota"
      "nf_log_syslog"
      "ip_tables"
      "ip6_tables"
      "iptable_filter"
      "ipt_rpfilter"
      "ip6t_rpfilter"
      "ipt_REJECT"
      "ip6t_REJECT"
      "iptable_nat"
      "iptable_mangle"
      "ip6table_filter"
      "ip6table_nat"
      "ip6table_mangle"
      "x_tables"
      "nf_nat"
      "nf_conntrack"
      "nf_defrag_ipv4"
      "nf_defrag_ipv6"
      "nf_reject_ipv4"
      "nf_reject_ipv6"
      "xt_addrtype"
      "xt_conntrack"
      "xt_MASQUERADE"
      "xt_comment"
      "xt_owner"
      "xt_mark"
      "xt_multiport"
      "xt_tcpudp"
      "xt_nat"
      "xt_NFLOG"
      "xt_LOG"
      "xt_CHECKSUM"
      "xt_hl"
      "xt_limit"
      "xt_connmark"
      "xt_connlabel"
      "xt_physdev"
      "xt_pkttype"
      "xt_CT"
      "xt_set"
      "ip_set"
      "ib_core"
      "rdma_cm"
      "kvm"
      "kvm_intel"
      "kvm_amd"
      "kheaders"
      "configs"
    ]
  );
  kernelEnvironment = pkgs.stdenv // {
    mkDerivation =
      args:
      let
        patch =
          attrs:
          attrs
          // pkgs.lib.optionalAttrs ((attrs.pname or "") == "linux-config") {
            generateConfig = kernelConfig;
            postPatch = (attrs.postPatch or "") + ''
              ${pkgs.buildPackages.perl}/bin/perl lib/build_OID_registry \
                include/linux/oid_registry.h oid-registry-check.c
              ${pkgs.buildPackages.python3}/bin/python ${./tests/kernel-oid-registry.py} \
                include/linux/oid_registry.h oid-registry-check.c
            '';
            outputs = (attrs.outputs or [ "out" ]) ++ [ "fullConfig" ];
            installPhase = ''
              cp "$buildRoot/.config" "$fullConfig"
              source_root="$PWD"
              (
                cd "$buildRoot"
                ARCH="$kernelArch" SRCARCH=${
                  if pkgs.stdenv.hostPlatform.isx86 then "x86" else pkgs.stdenv.hostPlatform.linuxArch
                } \
                  LSMOD=${kernelModules} perl "$source_root/scripts/kconfig/streamline_config.pl" \
                  --localmodconfig "$source_root" Kconfig > .config.selected
              )
              mv "$buildRoot/.config.selected" "$buildRoot/.config"
              make "''${makeFlags[@]}" -C . O="$buildRoot" olddefconfig
              protected='(SECURITY.*|SECCOMP.*|LSM.*|NAMESPACES|USER_NS|PID_NS|NET_NS|IPC_NS|UTS_NS|CGROUP_NS|HARDENED.*|FORTIFY_SOURCE|STACKPROTECTOR.*|RANDOMIZE.*|RANDOM_KMALLOC_CACHES|INIT_STACK_ALL.*|ZERO_CALL_USED_REGS|STRICT_KERNEL_RWX|STRICT_MODULE_RWX|STRICT_DEVMEM|IO_STRICT_DEVMEM|PAGE_TABLE_ISOLATION|CPU_MITIGATIONS|X86_SMAP|X86_SMEP|MODULE_SIG.*|LOCK_DOWN.*|LOCKDEP.*|VMAP_STACK)'
              grep -E "^CONFIG_$protected=" "$fullConfig" | sort > protected.before
              grep -E "^CONFIG_$protected=" "$buildRoot/.config" | sort > protected.after
              diff -u protected.before protected.after
              ${attrs.installPhase}
            '';
          };
      in
      pkgs.stdenv.mkDerivation (
        if builtins.isFunction args then final: patch (args final) else patch args
      );
  };
  kernelPackages = pkgs.linuxPackages_7_2.extend (
    _final: previous: {
      kernel = previous.kernel.override {
        stdenv = kernelEnvironment;
      };
    }
  );
  guestProgram = pkgs.writeShellApplication {
    name = "loom-native-kernel-program";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      IFS= read -r outer_boot < /run/loom-report/outer-boot-id
      IFS= read -r inner_boot < /proc/sys/kernel/random/boot_id
      [[ "$outer_boot" != "$inner_boot" ]]
      [[ ! -e /run/loom-report/kernel-state/host-only ]]
      [[ ! -s /run/loom-report/boot-failed-units ]]
      [[ -r ${pkgs.bash}/bin/bash && ! -w ${pkgs.bash}/bin/bash ]]
      no_privileges=0
      groups_empty=0
      while IFS=: read -r key value; do
        if [[ "$key" == NoNewPrivs && "$value" == *1 ]]; then no_privileges=1; fi
        if [[ "$key" == Groups && "$value" =~ ^[[:space:]]*$ ]]; then groups_empty=1; fi
      done < /proc/self/status
      [[ "$no_privileges" == 1 && "$groups_empty" == 1 ]]
      IFS= read -r group_policy < /proc/self/setgroups
      [[ "$group_policy" == allow ]]
      for kind in uid gid; do
        mapfile -t mapping < "/proc/self/''${kind}_map"
        [[ "''${#mapping[@]}" == 1 ]]
        read -r inside outside size extra <<< "''${mapping[0]}"
        [[ "$inside" == 0 && "$outside" == 0 && "$size" == 1 && -z "$extra" ]]
      done
      printf 'native-kernel caller: zero supplementary groups, one-ID UID/GID maps, setgroups allowed\n' >&2
      sha256sum ${guest.config.boot.kernelPackages.kernel.configfile.fullConfig} \
        ${guest.config.boot.kernelPackages.kernel.configfile} >&2
      printf 'native-kernel boundary: distinct boot, protected input, hidden host marker, no-new-privileges\n' >&2
      export LOOM_SMOKE_REPORT_DIR=/run/loom-report/retained
      exec ${escapeShellArg program}
    '';
  };
  guestSupervisor = pkgs.writeShellApplication {
    name = "loom-native-kernel-supervisor";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.systemd
      pkgs.util-linux
    ];
    text = ''
      report=/run/loom-guest-report
      journalctl --boot --unit=firewall --no-pager > "$report/firewall-journal"
      systemctl --failed --no-pager --plain --no-legend > "$report/boot-failed-units"
      group=/sys/fs/cgroup/system.slice/loom-native-guest.service
      projector=${storeProjection}/bin/project-store
      mkdir /run/loom-input-store
      mount --bind /nix/store /run/loom-input-store
      properties=(
        '--property=Delegate=yes'
        '--property=DelegateSubgroup=supervisor'
        '--property=RuntimeMaxSec=840'
        '--property=TimeoutStopSec=10'
        '--property=KillMode=control-group'
        '--property=ProtectSystem=strict'
        '--property=ProtectHome=tmpfs'
        '--property=PrivateTmp=yes'
        '--property=PrivateNetwork=yes'
        '--property=PrivateDevices=yes'
        '--property=DevicePolicy=closed'
        '--property=DeviceAllow=/dev/fuse rw'
        '--property=DeviceAllow=/dev/net/tun rw'
        '--property=PrivatePIDs=yes'
        '--property=PrivateIPC=yes'
        '--property=ProtectControlGroups=yes'
        '--property=ProtectKernelTunables=yes'
        '--property=NoNewPrivileges=yes'
        '--property=UMask=0077'
        '--property=TemporaryFileSystem=/run /nix/store'
        '--property=InaccessiblePaths=-/nix/var -/etc/wrix'
        '--property=BindReadOnlyPaths=/sys /dev/null:/etc/subuid /dev/null:/etc/subgid /run/loom-input-store'
        "--property=ReadWritePaths=$group /proc/sys/net"
        "--property=BindPaths=$report:/run/loom-report $group:$group /dev/fuse /dev/net/tun"
      )
      status=0
      systemd-run --system --quiet --wait --pipe --collect --expand-environment=no \
        --unit=loom-native-guest "''${properties[@]}" \
        -- "/run/loom-input-store/''${projector#/nix/store/}" \
        ${pkgs.coreutils}/bin/env -i \
        out=/run/loom-report \
        LOOM_NATIVE_CGROUP_PARENT=/system.slice/loom-native-guest.service \
        LOOM_NATIVE_NIX_REGISTRATION=/run/loom-report/native-input-registration \
        ${pkgs.util-linux}/bin/setpriv --clear-groups -- \
        ${pkgs.util-linux}/bin/unshare --user --map-users=0:0:1 --map-groups=0:0:1 \
        --setgroups=allow --mount --net --cgroup --fork -- \
        ${pkgs.util-linux}/bin/setpriv --clear-groups -- \
        ${guestProgram}/bin/loom-native-kernel-program > "$report/guest-stdout" 2> "$report/guest-stderr" || status=$?
      printf '%s\n' "$status" > "$report/guest-status"
      sync -f "$report/guest-status"
      systemctl poweroff --no-block
    '';
  };
  guest = import (pkgs.path + "/nixos/lib/eval-config.nix") {
    inherit system;
    modules = [
      (
        { ... }:
        {
          nixpkgs.pkgs = pkgs;
          system.stateVersion = "26.05";
          networking.hostName = "loom-native-kernel";
          networking.useDHCP = false;
          nix.enable = false;
          documentation.enable = false;
          boot.loader.grub.enable = false;
          boot.kernelPackages = kernelPackages;
          boot.kernelPatches = [
            {
              name = "kconfig-immutable-shell";
              patch = pkgs.replaceVars ./patches/linux-kconfig-shell.patch {
                bash = "${pkgs.bash}/bin/bash";
              };
            }
          ];
          boot.consoleLogLevel = 7;
          boot.kernelParams = [
            "console=ttyS0"
            "panic=1"
            "boot.panic_on_fail"
          ];
          boot.initrd.includeDefaultModules = false;
          boot.initrd.availableKernelModules = [
            "virtio_pci"
            "virtio_blk"
            "virtio_rng"
            "9p"
            "9pnet_virtio"
          ];
          boot.kernelModules = [
            "fuse"
            "tun"
            "overlay"
            "nf_tables"
          ];
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          fileSystems."/nix/store" = {
            device = "loomstore";
            fsType = "9p";
            options = [
              "trans=virtio"
              "version=9p2000.L"
              "ro"
              "cache=loose"
              "msize=1048576"
            ];
            neededForBoot = true;
          };
          fileSystems."/run/loom-guest-report" = {
            device = "loomreport";
            fsType = "9p";
            options = [
              "trans=virtio"
              "version=9p2000.L"
              "msize=1048576"
            ];
          };
          systemd.services.loom-native-kernel = {
            wantedBy = [ "multi-user.target" ];
            after = [
              "local-fs.target"
              "firewall.service"
            ];
            requires = [ "firewall.service" ];
            unitConfig.RequiresMountsFor = [ "/run/loom-guest-report" ];
            onFailure = [ "poweroff.target" ];
            serviceConfig = {
              Type = "oneshot";
              ExecStart = "${guestSupervisor}/bin/loom-native-kernel-supervisor";
              TimeoutStartSec = 860;
            };
          };
        }
      )
    ];
  };
  image = guest.config.system.build.toplevel;
  machine = if pkgs.stdenv.hostPlatform.isAarch64 then "virt,accel=kvm" else "accel=kvm";
in
pkgs.writeShellApplication {
  name = "loom-native-kernel";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.e2fsprogs
  ];
  text = ''
    out="''${out:?native report directory required}"
    : "''${LOOM_NATIVE_NIX_REGISTRATION:?admitted native inputs required}"
    [[ ! -e "$out/guest" && ! -e "$out/kernel-state" ]]
    mkdir "$out/guest" "$out/kernel-state"
    cp "$LOOM_NATIVE_NIX_REGISTRATION" "$out/guest/native-input-registration"
    cp /proc/sys/kernel/random/boot_id "$out/guest/outer-boot-id"
    printf 'host-only marker\n' > "$out/kernel-state/host-only"
    sha256sum "$out/kernel-state/host-only" > "$out/kernel-state/host-only.sha256"
    disk="$out/kernel-state/root.raw"
    [[ ! -e "$disk" ]]
    truncate -s 64G "$disk"
    mkfs.ext4 -q -F -L loom-native-root "$disk"
    hypervisor_status=0
    ${pkgs.qemu}/bin/qemu-system-${pkgs.stdenv.hostPlatform.qemuArch} \
      -no-user-config -nodefaults -no-reboot \
      -machine ${escapeShellArg machine} -cpu host -m 6144 -smp 2 \
      -sandbox on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny \
      -display none -monitor none -serial "file:$out/guest-console" \
      -nic none -device virtio-rng-pci \
      -kernel ${image}/kernel -initrd ${image}/initrd \
      -append "init=${image}/init $(cat ${image}/kernel-params)" \
      -drive "file=$disk,format=raw,if=virtio,cache=writeback" \
      -fsdev local,id=loomstore,path=/nix/store,security_model=none,readonly=on \
      -device virtio-9p-pci,fsdev=loomstore,mount_tag=loomstore \
      -fsdev "local,id=loomreport,path=$out/guest,security_model=none" \
      -device virtio-9p-pci,fsdev=loomreport,mount_tag=loomreport \
      > "$out/qemu-stdout" 2> "$out/qemu-stderr" || hypervisor_status=$?
    sha256sum --check "$out/kernel-state/host-only.sha256" >&2
    if [[ "$hypervisor_status" != 0 || ! -f "$out/guest/guest-status" ]]; then
      cat "$out/qemu-stderr" >&2
      if [[ -f "$out/guest-console" ]]; then cat "$out/guest-console" >&2; fi
      exit 1
    fi
    [[ ! -L "$out/guest/guest-status" && "$(stat -c %s "$out/guest/guest-status")" -le 4 ]]
    IFS= read -r status < "$out/guest/guest-status"
    [[ "$status" =~ ^(0|[1-9][0-9]{0,2})$ && "$status" -le 255 ]]
    [[ "$(stat -c %s "$out/guest/guest-status")" == "$((''${#status} + 1))" ]]
    if [[ "$status" == 0 ]]; then
      # Only this invocation's working disk is retired, after lossless retention.
      # Existing retained disks and roots are never touched.
      retained="$out/kernel-state/guest.qcow2"
      stat --printf='%n\t%s\t%b\t%a\t%u\t%g\t%y\n' "$disk" > "$out/kernel-state/working-disk.stat"
      ${pkgs.qemu}/bin/qemu-img convert -f raw -O qcow2 -c -o compression_type=zstd \
        "$disk" "$retained" > "$out/kernel-state/convert.log" 2>&1
      ${pkgs.qemu}/bin/qemu-img compare -f raw -F qcow2 "$disk" "$retained" \
        > "$out/kernel-state/compare.log" 2>&1
      ${pkgs.qemu}/bin/qemu-img info --output=json "$retained" > "$out/kernel-state/retained-disk.json"
      rm -- "$disk"
    fi
    cat "$out/guest/guest-stdout"
    cat "$out/guest/guest-stderr" >&2
    exit "$status"
  '';
}
