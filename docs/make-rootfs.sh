#!/bin/sh
# Ensambla el rootfs minimo para WSL: busybox + glibc + wrapper + libs Android.
set -e
OUT=/out
rm -rf $OUT/* 2>/dev/null || true
apt-get update -qq >/dev/null && apt-get install -y -qq busybox-static >/dev/null

mkdir -p $OUT/bin $OUT/lib/x86_64-linux-gnu $OUT/lib64 $OUT/usr/bin \
         $OUT/proc $OUT/sys $OUT/dev $OUT/tmp $OUT/etc $OUT/root $OUT/run $OUT/mnt $OUT/app

cp /bin/busybox $OUT/bin/busybox
for a in sh ash stat chown chmod mkdir rmdir rm cat echo printf sleep ps kill ls ln \
         grep sed awk date id env touch cp mv tar gzip du df head tail wc which \
         mount umount hostname uname sync true false test dirname basename find xargs; do
    ln -sf busybox $OUT/bin/$a
done

# glibc para el binario externo `wrapper` (busybox es estatico, wrapper no)
cp -L /lib/x86_64-linux-gnu/libc.so.6 $OUT/lib/x86_64-linux-gnu/
cp -L /lib64/ld-linux-x86-64.so.2 $OUT/lib64/
for l in libnss_dns.so.2 libnss_files.so.2 libresolv.so.2; do
    [ -e /lib/x86_64-linux-gnu/$l ] && cp -L /lib/x86_64-linux-gnu/$l $OUT/lib/x86_64-linux-gnu/ || true
done

# el wrapper y las libs de Android
cp -a /app/wrapper $OUT/app/wrapper
cp -a /app/rootfs $OUT/app/rootfs

printf 'root:x:0:0:root:/root:/bin/sh\n' > $OUT/etc/passwd
printf 'root:x:0:\n' > $OUT/etc/group
printf 'hosts: files dns\n' > $OUT/etc/nsswitch.conf
printf 'nameserver 1.1.1.1\nnameserver 1.0.0.1\n' > $OUT/etc/resolv.conf
printf '127.0.0.1 localhost\n' > $OUT/etc/hosts
: > $OUT/etc/fstab   # sin esto WSL escupe 'Processing /etc/fstab with mount -a failed'

cat > $OUT/etc/wsl.conf <<'WSLCONF'
[automount]
enabled = false
mountFsTab = false
[network]
generateResolvConf = true
hostname = ecam
[boot]
systemd = false
[user]
default = root
[interop]
enabled = false
appendWindowsPath = false
WSLCONF

du -sh $OUT
