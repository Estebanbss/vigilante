# Vigilante - Sistema de Vigilancia con SSE

## ✅ Seguridad Total

**TODAS las rutas ahora requieren autenticación por token.**

El sistema usa un **middleware flexible** que valida token tanto por:
- **Header**: `Authorization: Bearer TOKEN`
- **Query**: `?token=TOKEN` (útil para frontend)

## Autenticación

### Todas las rutas requieren token:

**Opción 1 - Header (recomendado para APIs):**
```bash
curl -H "Authorization: Bearer YOUR_TOKEN" http://localhost:3000/api/storage/stream
```

**Opción 2 - Query parameter (fácil para frontend):**
```bash
curl "http://localhost:3000/api/storage/stream?token=YOUR_TOKEN"
```

### Rutas Disponibles:

- `/hls/*` - HLS streaming (requiere token)
- `/webrtc/*` - WebRTC streaming (requiere token)
- `/api/live/mjpeg` - MJPEG streaming (requiere token)
- `/api/live/audio` - Audio streaming (requiere token)
- `/api/logs/stream` - Logs del sistema (requiere token)
- `/api/storage/stream` - Info de almacenamiento (requiere token)
- `/api/recordings/stream` - Lista de grabaciones (requiere token)
- `/api/recordings/stream/tail/*path` - Video streaming (requiere token)
- `/api/storage` - Gestión de almacenamiento (requiere token)
- `/api/ptz/*` - Control PTZ (requiere token)

## Configuración

```bash
cp .env.example .env
# Configurar PROXY_TOKEN en .env
```

## Cambios Realizados

Se cambió la implementación de WebSocket a **Server-Sent Events (SSE)** para las rutas de storage y recordings, siguiendo el mismo patrón que los logs del journal.

### Ventajas de SSE sobre WebSocket:

1. **Unidireccional**: Perfecto para datos que solo van del servidor al cliente
2. **Sin polling**: Push inmediato cuando hay cambios
3. **HTTP puro**: Mejor compatibilidad con proxies y firewalls
4. **Más simple**: No necesita handshake complejo
5. **Menos recursos**: No mantiene conexiones bidireccionales innecesarias

### Rutas Cambiadas:

- `GET /api/storage/stream` → SSE (antes WebSocket)
- `GET /api/recordings/stream` → SSE (antes WebSocket)
- `GET /api/logs/stream` → SSE (ya era SSE)
- `GET /api/recordings/stream/tail/*path?token=...` → Token por query (antes auth global)

## Implementación en Flutter

### 1. Dependencias

Agrega al `pubspec.yaml`:

```yaml
dependencies:
  flutter:
    sdk: flutter
  http: ^1.1.0
```

### 2. Servicio SSE

```dart
import 'dart:async';
import 'dart:convert';
import 'package:http/http.dart' as http;

class VigilanteService {
  final String baseUrl;
  final String? token;

  VigilanteService({required this.baseUrl, this.token});

  Stream<Map<String, dynamic>> _connectToSSE(String endpoint) async* {
    final url = Uri.parse('$baseUrl$endpoint');
    if (token != null) {
      url.replace(queryParameters: {'token': token});
    }

    final request = http.Request('GET', url);
    request.headers['Accept'] = 'text/event-stream';
    request.headers['Cache-Control'] = 'no-cache';

    final response = await http.Client().send(request);

    if (response.statusCode != 200) {
      throw Exception('Error conectando: ${response.statusCode}');
    }

    await for (final line in response.stream.transform(utf8.decoder).transform(LineSplitter())) {
      if (line.startsWith('data: ')) {
        final jsonStr = line.substring(6);
        if (jsonStr.isNotEmpty) {
          yield jsonDecode(jsonStr);
        }
      }
    }
  }

  // Storage info stream
  Stream<StorageInfo> storageStream() => _connectToSSE('/api/storage/stream')
      .map((json) => StorageInfo.fromJson(json));

  // Recordings list stream
  Stream<List<Recording>> recordingsStream() => _connectToSSE('/api/recordings/stream')
      .map((json) => (json as List).map((item) => Recording.fromJson(item)).toList());

  // Journal logs stream
  Stream<String> journalLogsStream() => _connectToSSE('/api/logs/stream')
      .map((json) => json.toString());
}
```

### 3. Modelo de Datos

```dart
class StorageInfo {
  final int totalSpaceBytes;
  final int usedSpaceBytes;
  final String storagePath;
  final String storageName;

  StorageInfo({
    required this.totalSpaceBytes,
    required this.usedSpaceBytes,
    required this.storagePath,
    required this.storageName,
  });

  factory StorageInfo.fromJson(Map<String, dynamic> json) => StorageInfo(
    totalSpaceBytes: json['total_space_bytes'],
    usedSpaceBytes: json['used_space_bytes'],
    storagePath: json['storage_path'],
    storageName: json['storage_name'],
  );
}

class Recording {
  final String filename;
  final String path;
  final int size;
  final DateTime created;
  final DateTime modified;

  Recording({
    required this.filename,
    required this.path,
    required this.size,
    required this.created,
    required this.modified,
  });

  factory Recording.fromJson(Map<String, dynamic> json) => Recording(
    filename: json['filename'],
    path: json['path'],
    size: json['size'],
    created: DateTime.parse(json['created']),
    modified: DateTime.parse(json['modified']),
  );
}
```

### 4. Widget de Ejemplo

```dart
class StorageWidget extends StatefulWidget {
  final VigilanteService service;

  const StorageWidget({Key? key, required this.service}) : super(key: key);

  @override
  _StorageWidgetState createState() => _StorageWidgetState();
}

class _StorageWidgetState extends State<StorageWidget> {
  StreamSubscription<StorageInfo>? _subscription;
  StorageInfo? _info;

  @override
  void initState() {
    super.initState();
    _subscription = widget.service.storageStream().listen((info) {
      setState(() => _info = info);
    });
  }

  @override
  void dispose() {
    _subscription?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_info == null) return const CircularProgressIndicator();

    final usedGB = (_info!.usedSpaceBytes / (1024*1024*1024)).toStringAsFixed(2);
    final totalGB = (_info!.totalSpaceBytes / (1024*1024*1024)).toStringAsFixed(2);
    final percent = (_info!.usedSpaceBytes / _info!.totalSpaceBytes * 100).toStringAsFixed(1);

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          children: [
            Text(_info!.storageName, style: Theme.of(context).textTheme.headline6),
            Text('$usedGB GB / $totalGB GB ($percent%)'),
            LinearProgressIndicator(value: _info!.usedSpaceBytes / _info!.totalSpaceBytes),
          ],
        ),
      ),
    );
  }
}
```

### 5. Manejo de Errores

```dart
Stream<T> _handleErrors<T>(Stream<T> stream) async* {
  try {
    await for (final item in stream) {
      yield item;
    }
  } catch (e) {
    print('Error en stream SSE: $e');
    // Reintentar conexión después de delay
    await Future.delayed(const Duration(seconds: 5));
    // Aquí podrías re-emitir el stream o manejar el error
  }
}
```

## Comparación WebSocket vs SSE

| Característica | WebSocket | SSE |
|---|---|---|
| Dirección | Bidireccional | Unidireccional (servidor → cliente) |
| Protocolo | ws:// | HTTP |
| Compatibilidad | Buena | Excelente (HTTP puro) |
| Complejidad | Alta | Baja |
| Uso de recursos | Más | Menos |
| Re-conexión | Manual | Automática |
| Navegadores | Todos modernos | Todos modernos |

## Autenticación

### Configuración de Seguridad

Las rutas públicas pueden requerir token opcionalmente mediante la variable de entorno:

```bash
# Si es true, las rutas públicas requieren ?token=TOKEN
STREAM_TOKEN_IN_QUERY=true
# o
STREAM_MJPEG_TOKEN_IN_QUERY=true
```

### Rutas Públicas (sin token por defecto):
- `/api/logs/stream` - Logs del sistema
- `/api/storage/stream` - Información de almacenamiento
- `/api/recordings/stream` - Lista de grabaciones

**Si `STREAM_TOKEN_IN_QUERY=true`, estas rutas requieren `?token=TOKEN`**

### Rutas con Token por Query (siempre requieren token):
- `/api/recordings/stream/tail/*path?token=TOKEN` - Streaming de video en tiempo real

### Rutas con Auth Global:
- Todas las demás rutas API requieren header `Authorization: Bearer TOKEN`

## URLs de Ejemplo

### Con Header Authorization:
```bash
# Storage info
curl -H "Authorization: Bearer YOUR_TOKEN" \
     -H "Accept: text/event-stream" \
     http://localhost:3000/api/storage/stream

# Recordings list
curl -H "Authorization: Bearer YOUR_TOKEN" \
     -H "Accept: text/event-stream" \
     http://localhost:3000/api/recordings/stream

# Journal logs
curl -H "Authorization: Bearer YOUR_TOKEN" \
     -H "Accept: text/event-stream" \
     http://localhost:3000/api/logs/stream
```

### Con Token por Query (fácil para frontend):
```bash
# Storage info
curl -H "Accept: text/event-stream" \
     "http://localhost:3000/api/storage/stream?token=YOUR_TOKEN"

# Recordings list
curl -H "Accept: text/event-stream" \
     "http://localhost:3000/api/recordings/stream?token=YOUR_TOKEN"

# Journal logs
curl -H "Accept: text/event-stream" \
     "http://localhost:3000/api/logs/stream?token=YOUR_TOKEN"

# Video streaming
curl "http://localhost:3000/api/recordings/stream/tail/2024-01-15/camera1.mp4?token=YOUR_TOKEN"
```