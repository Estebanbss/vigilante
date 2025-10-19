#!/bin/bash
# filepath: /home/teban/Proyectos/vigilante/fix_drive.sh

MOUNT_POINT="/media/teban/grabaciones"
DEVICE="/dev/sda1"

# Función para loggear
log() {
    echo "$(date): $1" | tee -a /var/log/fix_drive.log
}

log "Iniciando verificación del disco $DEVICE"

# Verifica si está montado
if mount | grep -q "$DEVICE on $MOUNT_POINT"; then
    log "El disco ya está montado en $MOUNT_POINT"
else
    log "El disco no está montado. Intentando montar..."
    sudo mkdir -p $MOUNT_POINT
    if sudo mount $DEVICE $MOUNT_POINT; then
        log "Disco montado exitosamente"
    else
        log "Fallo al montar. Intentando reparar con fsck..."
        sudo umount $DEVICE 2>/dev/null || true
        if sudo fsck -y $DEVICE; then
            log "Reparación completada. Intentando montar de nuevo..."
            if sudo mount $DEVICE $MOUNT_POINT; then
                log "Disco montado después de reparación"
            else
                log "Error: No se pudo montar después de reparación"
                exit 1
            fi
        else
            log "Error: fsck falló. Disco posiblemente dañado físicamente"
            exit 1
        fi
    fi
fi

# Verifica permisos (opcional, pero útil)
if [ -d "$MOUNT_POINT" ]; then
    sudo chown -R teban:teban $MOUNT_POINT
    sudo chmod -R 755 $MOUNT_POINT
    log "Permisos ajustados"
fi

# Reinicia vigilante si está corriendo
if sudo systemctl is-active --quiet vigilante.service; then
    log "Reiniciando servicio vigilante..."
    sudo systemctl restart vigilante.service
else
    log "Servicio vigilante no está activo"
fi

log "Proceso completado"