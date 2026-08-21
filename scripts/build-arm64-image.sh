#!/bin/sh
# Construye la imagen del motor de ECAM para Apple Silicon.
#
# Se puede correr desde una máquina x86: el Dockerfile es solo COPY, no compila
# nada. El `wrapper`, el `linker64` y las 99 `.so` (`libCoreLSKD` incluida) vienen
# ya compilados para aarch64 del release oficial de WorldObservationLog.
#
# Salida: ecam-arm64-image.tar.gz (~78 MB), que la app carga con `docker load`
# desde la pantalla de preparar el motor.
set -e

OUT="${1:-$PWD/ecam-arm64-image.tar.gz}"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "==> bajando el release arm64"
gh release download wrapper.arm64.latest \
  --repo WorldObservationLog/wrapper --dir "$WORK" --clobber
unzip -q "$WORK"/Wrapper.arm64.latest.zip -d "$WORK/src"

echo "==> comprobando que es de verdad aarch64"
for f in wrapper rootfs/system/bin/main rootfs/system/bin/linker64 \
         rootfs/system/lib64/libCoreLSKD.so; do
  file -b "$WORK/src/$f" | grep -q "ARM aarch64" \
    || { echo "!! $f no es aarch64"; exit 1; }
done

mkdir -p "$WORK/ctx/app"
cp -a "$WORK/src/wrapper" "$WORK/src/wrapper-rootless" \
      "$WORK/src/entrypoint.sh" "$WORK/ctx/app/"
cp -a "$WORK/src/rootfs" "$WORK/ctx/app/rootfs"
# La sesión NUNCA va dentro de la imagen: viaja por el volumen del host, para
# que sobreviva a borrar y recrear el contenedor.
rm -rf "$WORK/ctx/app/rootfs/data"
mkdir -p "$WORK/ctx/app/rootfs/data"

cat > "$WORK/ctx/Dockerfile" <<'DOCKER'
FROM debian:13-slim
COPY app /app
WORKDIR /app
EXPOSE 10020 20020 30020
CMD ["/bin/sh", "-c", "cd /app && exec ./wrapper -H 0.0.0.0"]
DOCKER

echo "==> construyendo"
docker build --platform linux/arm64 -t ecam:arm64 "$WORK/ctx"

arch=$(docker image inspect ecam:arm64 --format '{{.Architecture}}')
[ "$arch" = "arm64" ] || { echo "!! la imagen salió $arch"; exit 1; }

echo "==> exportando a $OUT"
docker save ecam:arm64 | gzip -9 > "$OUT"
ls -lh "$OUT"
