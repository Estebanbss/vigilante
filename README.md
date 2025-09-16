# Vigilante

Sistema de vigilancia en Rust con:
- Grabación continua (segmentada) desde RTSP usando GStreamer
- API de almacenamiento (listar, borrar, reproducir)
- Detección básica de eventos (mock) y logs por fecha
- Streaming en vivo separado (MJPEG) vía HTTP
- Streaming HLS en vivo (playlist .m3u8 + segmentos .ts)
- Endpoints PTZ ONVIF reales (ContinuousMove/Stop)

## Requisitos del sistema
Para que los pipelines de GStreamer funcionen, instala GStreamer y plugins:

Debian/Ubuntu:
```bash
sudo apt update
sudo apt install -y gstreamer1.0-tools gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

Fedora:
```bash
sudo dnf install -y gstreamer1-plugins-base gstreamer1-plugins-good \
  gstreamer1-plugins-bad-free gstreamer1-plugins-ugly gstreamer1-libav \
  gstreamer1-plugins-bad-freeworld
```

Arch:
```bash
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav
```

Nota: Si ves errores como `no element "avdec_h264"`, te falta el paquete `gstreamer1.0-libav` (o equivalente de tu distro).

## Variables de entorno
Crea un archivo `.env` (ya hay un ejemplo en el repo):
```
CAMERA_RTSP_URL=rtsp://usuario:password@IP:PUERTO/stream1
CAMERA_ONVIF_URL=http://usuario:password@IP:PUERTO/onvif/ptz
PROXY_TOKEN=mi_token_seguro
LISTEN_ADDR=0.0.0.0:8080
STORAGE_PATH=/ruta/al/almacenamiento
```

## Ejecutar
```bash
cargo run
```

## Endpoints
Autenticación: Header `Authorization: Bearer ${PROXY_TOKEN}`
También se acepta `?token=${PROXY_TOKEN}` en URLs de streaming (compatibilidad con players).

- GET `/api/storage` → Info de disco
- GET `/api/storage/list?date=YYYY-MM-DD&page=1&limit=20` → Lista grabaciones
- GET `/api/storage/delete/*path` → Borra archivo (path relativo al STORAGE_PATH)
- GET `/api/recordings/stream/*path` → Stream mp4 inline
- GET `/api/recordings/log/:date` → Lee log diario
- GET `/api/live/mjpeg` → Streaming en vivo MJPEG (separado de la grabación)
- GET `/hls/stream.m3u8` → Playlist HLS (segmentos en `/hls/segment-xxxxx.ts`)
- POST `/api/ptz/...` → PTZ ONVIF (pan/tilt/zoom y stop)

### Probar streaming en vivo (MJPEG)
Navegador:
- Abre: `http://HOST:8080/api/live/mjpeg` con el header Authorization (usa una extensión tipo ModHeader) 

cURL (guarda frames JPEG concatenados):
```bash
curl -H "Authorization: Bearer $PROXY_TOKEN" \
  http://localhost:8080/api/live/mjpeg --output - | head -c 4096 > sample.bin
```

### Probar HLS en vivo
Una vez en ejecución, el servidor genera `STORAGE_PATH/hls/stream.m3u8` y segmentos `.ts`.

Reproducir con VLC:
```bash
vlc "http://localhost:8080/hls/stream.m3u8?token=$PROXY_TOKEN"
```

o con cURL para inspección:
```bash
curl "http://localhost:8080/hls/stream.m3u8?token=$PROXY_TOKEN"
```

## Notas técnicas
- Grabación: Un archivo MP4 diario (`YYYY-MM-DD.mp4`) que se actualiza en tiempo real. A medianoche se crea automáticamente un nuevo archivo para el día siguiente.
- Detección de eventos: se ejecuta sobre frames del branch de análisis (appsink) y escribe en `YYYY-MM-DD-log.txt`.
- Live MJPEG: se crea un pipeline independiente que re-encoda a JPEG y se sirve como `multipart/x-mixed-replace`.
- Live HLS: pipeline independiente con `hlssink2` + `mpegtsmux` que publica `stream.m3u8` y segmentos `.ts` bajo `STORAGE_PATH/hls`.
- PTZ ONVIF: SOAP 1.2 con UsernameToken (PasswordText) y BasicAuth; comandos implementados: ContinuousMove (pan/tilt/zoom) y Stop.

## Roadmap próximo
- Presets PTZ (SetPreset/GotoPreset) y velocidades configurables
- WebRTC (opcional) con proxy RTSP→RTC
- Tests básicos y manejo de errores mejorado

## Flutter (guía rápida)
- HLS recomendado en Flutter (video_player/chewie/better_player):
  - URL: `http://HOST:8080/hls/stream.m3u8?token=$PROXY_TOKEN`
  - Si tu player no pasa headers a sub-requests, usa el `?token=` en vez de header.
- MJPEG como alternativa (latencia baja, mayor consumo):
  - URL: `http://HOST:8080/api/live/mjpeg?token=$PROXY_TOKEN`
