# WebRTC Streaming Test

Esta página HTML permite probar la funcionalidad de streaming WebRTC con audio y video en ultra baja latencia.

## Requisitos

- Token de autenticación válido para `stream.estebanbss.dev`
- Navegador moderno con soporte WebRTC
- Conexión a internet

## Cómo usar

1. **Abrir la página**: Abre `test_webrtc_av.html` en tu navegador
2. **Token automático**: El token se carga automáticamente desde el archivo `.env` (PROXY_TOKEN)
3. **Conectar**: Haz click en "Connect WebRTC"
4. **Ver stream**: El video debería aparecer automáticamente con latencia < 500ms
5. **Desconectar**: Usa el botón "Disconnect" cuando termines

## Características

- ✅ Streaming de video H.264
- ✅ Audio Opus
- ✅ Latencia ultra baja (< 500ms)
- ✅ Autenticación Bearer token
- ✅ Medición de latencia en tiempo real
- ✅ Conexión peer-to-peer WebRTC

## Solución de problemas

- **Error de conexión**: Verifica que el token sea válido y que el servidor esté ejecutándose
- **No hay video/audio**: Espera unos segundos después de conectar, el stream puede tardar en inicializarse
- **Latencia alta**: Verifica tu conexión a internet y proximidad al servidor

## Endpoints del servidor

- `POST /api/webrtc/offer` - Iniciar conexión WebRTC
- `POST /api/webrtc/answer/{client_id}` - Responder a oferta
- `POST /api/webrtc/close/{client_id}` - Cerrar conexión

## Notas técnicas

- Usa STUN server de Google para NAT traversal
- Implementa medición de latencia usando WebRTC stats API
- Compatible con navegadores modernos (Chrome, Firefox, Safari, Edge)