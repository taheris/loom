final: prev: {
  nix =
    if final.stdenv.hostPlatform.isLinux then
      (prev.nix.appendPatches [ ./patches/nix-pid-only-procfs.patch ]).overrideScope (
        _finalScope: prevScope: {
          nix-store = prevScope.nix-store.overrideAttrs (old: {
            postPatch = (old.postPatch or "") + ''
              substituteInPlace unix/build/linux-derivation-builder.cc \
                --replace-fail 'if (store.config->getLocalSettings().requireDropSupplementaryGroups)' \
                'if (store.config->getLocalSettings().requireDropSupplementaryGroups && getgroups(0, 0) != 0)'
            '';
          });
          nix-functional-tests = prevScope.nix-functional-tests.overrideAttrs (old: {
            postPatch = (old.postPatch or "") + ''
              substituteInPlace supplementary-groups.sh \
                --replace-fail '  cmd=(' '  expectGroupPolicy () {
                  if grep -q "^Groups:[[:space:]]*$" /proc/self/status; then
                    "\$@"
                  else
                    expectStderr 1 "\$@" | grepQuiet "setgroups failed"
                  fi
                }
                cmd=(' \
                --replace-fail 'expectStderr 1 "\''${cmd[@]}" | grepQuiet "setgroups failed"' \
                  'expectGroupPolicy "\''${cmd[@]}"' \
                --replace-fail '# Fails with default setting' '# Enforces default group policy' \
                --replace-fail '# Fails with $(' '# Enforces $('
            '';
          });
          nix-store-test-support = prevScope.nix-store-test-support.overrideAttrs (old: {
            postPatch = (old.postPatch or "") + ''
              substituteInPlace test-main.cc \
                --replace-fail '#include <cstdlib>' '#include <cstdlib>
              #include <grp.h>
              #include <unistd.h>
              #include "nix/util/error.hh"' \
                --replace-fail 'settings.getWorkerSettings().buildHook = {};' \
                'settings.getWorkerSettings().buildHook = {};
                  settings.getLocalSettings().buildUsersGroup = "";
                  if (getgroups(0, nullptr) > 0 && setgroups(0, nullptr) == -1)
                      throw SysError("dropping supplementary groups before test namespaces");'
            '';
          });
        }
      )
    else
      prev.nix;
}
