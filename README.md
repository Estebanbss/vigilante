# Vigilante

Un sistema de vigilancia completo en Rust que proporciona grabación continua, streaming en vivo y control PTZ para cámaras IP.

## Características

- **Grabación Continua**: Captura video desde streams RTSP usando GStreamer con segmentación automática
- **Streaming en Vivo**: MJPEG para visualización en tiempo real
- **Control PTZ**: Integración completa con cámaras ONVIF para movimiento remoto
- **API REST**: Endpoints completos para gestión de grabaciones y control del sistema
- **Almacenamiento Organizado**: Estructura de archivos por fecha con metadatos
- **WebSocket**: Notificaciones en tiempo real del estado del sistema
- **Autenticación**: Sistema de tokens para acceso seguro

## Requisitos del Sistema

### GStreamer
Para el procesamiento de video, instala GStreamer y sus plugins:

**Ubuntu/Debian:**
```bash
sudo apt update
sudo apt install -y gstreamer1.0-tools gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

**Fedora:**
```bash
sudo dnf install -y gstreamer1-plugins-base gstreamer1-plugins-good \
  gstreamer1-plugins-bad-free gstreamer1-plugins-ugly gstreamer1-libav \
  gstreamer1-plugins-bad-freeworld
```

**Arch Linux:**
```bash
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav
```

### Rust
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Configuración

Crea un archivo `.env` en la raíz del proyecto:

```env
# URL del stream RTSP de la cámara
CAMERA_RTSP_URL=rtsp://usuario:password@IP:PUERTO/stream1

# URL ONVIF para control PTZ
CAMERA_ONVIF_URL=http://usuario:password@IP:PUERTO/onvif/ptz

# Token de autenticación para la API
PROXY_TOKEN=mi_token_seguro

# Dirección de escucha del servidor
LISTEN_ADDR=0.0.0.0:8080

# Directorio para almacenar grabaciones
STORAGE_PATH=/ruta/al/almacenamiento

# Permitir tokens en query parameters (útil para players sin headers)
STREAM_TOKEN_IN_QUERY=true
```

## Instalación y Ejecución

1. **Clona el repositorio:**
   ```bash
   git clone <repository-url>
   cd vigilante
   ```

2. **Configura las variables de entorno** (ver sección anterior)

3. **Compila el proyecto:**
   ```bash
   cargo build --release
   ```

4. **Ejecuta:**
   ```bash
   cargo run --release
   ```

El servidor estará disponible en `http://localhost:8080` (o la dirección configurada).

## API Documentation

### Autenticación
Todos los endpoints requieren autenticación mediante el header `Authorization: Bearer <PROXY_TOKEN>`.

### Endpoints

#### Sistema
- `GET /api/status` - Estado general del sistema
- `GET /api/metrics` - Métricas de rendimiento (Prometheus)

#### Cámara y Streaming
- `GET /stream/mjpeg` - Stream MJPEG en vivo
- `GET /stream/audio` - Stream de audio MP3

#### Control PTZ
- `POST /api/ptz/pan_left` - Mover cámara a la izquierda
- `POST /api/ptz/pan_right` - Mover cámara a la derecha
- `POST /api/ptz/tilt_up` - Inclinar cámara hacia arriba
- `POST /api/ptz/tilt_down` - Inclinar cámara hacia abajo
- `POST /api/ptz/zoom_in` - Acercar zoom
- `POST /api/ptz/zoom_out` - Alejar zoom
- `POST /api/ptz/stop` - Detener todos los movimientos

#### Almacenamiento
- `GET /api/recordings/summary` - Resumen de grabaciones por día
- `GET /api/recordings/day/{date}` - Lista detallada de grabaciones de un día específico
- `GET /api/recordings/{path}` - Streaming de grabación antigua
- `DELETE /api/recordings/{path}` - Eliminar grabación
- `GET /api/recordings/sse` - Eventos SSE de grabaciones

#### WebSocket
- `WS /ws` - Conexión WebSocket para frames MJPEG y comandos

### Ejemplos de Uso

#### Ver stream en vivo
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/stream/mjpeg
```

#### Mover cámara
```bash
curl -X POST \
     -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/ptz/pan_left
```

#### Obtener resumen de grabaciones
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/recordings/summary
```

Respuesta:
```json
[
  {
    "name": "2025-04-31",
    "records": 24
  },
  {
    "name": "2025-05-01",
    "records": 18
  }
]
```

## Arquitectura

### Componentes Principales

- **Camera Module**: Gestión de pipelines GStreamer y captura RTSP
- **Storage Module**: Organización y streaming de grabaciones
- **Stream Module**: Manejo de streams en vivo (MJPEG, WebSocket)
- **PTZ Module**: Control de cámaras ONVIF
- **Auth Module**: Autenticación y autorización
- **Metrics Module**: Recolección de métricas Prometheus
- **Logs Module**: Sistema de logging estructurado

### Estructura de Almacenamiento

```
STORAGE_PATH/
├── 2025-04-31/
│   ├── recording_001.mkv
│   ├── recording_002.mkv
│   └── ...
├── 2025-05-01/
│   └── ...
└── logs/
    ├── 2025-04-31.txt
    └── 2025-05-01.txt
```

## Desarrollo

### Ejecutar Tests
```bash
cargo test
```

### Verificar Código
```bash
cargo clippy
cargo fmt --check
```

### CI/CD
El proyecto incluye configuración de GitHub Actions para:
- Compilación automática
- Ejecución de tests
- Verificación de formato y linting
- Build de release

## Contribución

1. Fork el proyecto
2. Crea una rama para tu feature (`git checkout -b feature/nueva-funcionalidad`)
3. Commit tus cambios (`git commit -am 'Agrega nueva funcionalidad'`)
4. Push a la rama (`git push origin feature/nueva-funcionalidad`)
5. Abre un Pull Request

## Licencia

Este proyecto está bajo la Licencia MIT. Ver el archivo `LICENSE` para más detalles.

## Soporte

Para soporte técnico o reportar bugs, por favor abre un issue en el repositorio.
