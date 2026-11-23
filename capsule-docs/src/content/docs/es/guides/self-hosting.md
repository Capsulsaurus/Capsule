---
title: Autoalojamiento
description: Cómo ejecutar capsule-server sobre PostgreSQL y un almacén de blobs en el sistema de archivos
audit: none
---

Todavía no se ha publicado ninguna versión, así que el servidor se compila desde el código fuente.
El servidor solo almacena texto cifrado y nunca decodifica contenido multimedia; los clientes hacen
todo el procesamiento y el cifrado.

<a id="what-you-need"></a>

## Qué necesita

- **PostgreSQL 18**: el índice duradero (cuentas, recursos, cuota) y el estado efímero del servidor
  (sesiones iniciadas, sesiones de subida, contadores de frecuencia). Es la única base de datos que
  usa el servidor, y debe funcionar con su configuración de durabilidad intacta ([](#postgresql)).
- **Un directorio de blobs** (`BLOB_ROOT`) en **un único sistema de archivos POSIX** que respete
  `fsync` y comparta los bloqueos de archivos. Contiene los blobs y, en `ledger/`, el libro de
  registro firmado de cada propietario, a partir del cual se puede reconstruir el índice
  ([](#directorio-de-blobs)).
- **Un proxy inverso que termine TLS** delante del servidor y que deje pasar la compresión sin
  tocarla ([](#proxy-inverso)).
- **Un reloj sincronizado** en el host, mantenido por NTP ([](#reloj)).

### PostgreSQL

El servidor responde a una escritura en cuanto se confirma su fila, así que una confirmación que
PostgreSQL reconoce y luego pierde puede devolver a la vida una sesión revocada o aceptar dos veces un
código del segundo factor. Mantenga:

- `fsync = on` y `full_page_writes = on`, los valores predeterminados;
- `synchronous_commit` en `on`, `local`, `remote_write` o `remote_apply`, nunca en `off`;
- el directorio de datos en un almacenamiento que respete el vaciado a disco. Una caché de escritura
  de disco sin protección ante cortes de corriente, o un disco virtual cuyo host ignora los
  vaciados, pierde confirmaciones ya reconocidas en un corte de corriente
  ([](/design/filesystem/server/)).

PostgreSQL puede estar en un sistema de archivos, un disco o un host distintos de los de `BLOB_ROOT`.

<a id="blob-directory"></a>

### Directorio de blobs

`BLOB_ROOT` es un contrato con el sistema de archivos
([](/design/filesystem/server/#blob-root-filesystem-contract)), organizado como se describe en
[](/design/filesystem/server/#blob-store-layout):

- **Un único sistema de archivos.** La finalización es un renombrado atómico, así que el árbol
  completo, `incoming/` incluido, no puede repartirse entre volúmenes, datasets ni PersistentVolumes
  de Kubernetes.
- **`fsync` se respeta**, en archivos y en directorios, antes de que el servidor reconozca una
  escritura.
- **Los bloqueos de archivos se comparten** entre todos los procesos que escriben en el árbol.
  `serve` y `purge --apply` bloquean el `ledger/<owner>/.lock` de cada propietario antes de añadir
  entradas ([](/design/filesystem/server/#concurrent-appenders)).
- **Un único `serve` por directorio de blobs.** Los fragmentos de subida y la finalización se
  reclaman dentro del proceso que da servicio ([](/design/transfer/upload/)), así que no se admiten
  dos procesos `serve` sobre un mismo árbol, ni en un host ni en dos. `gc`, `purge` y `scrub` pueden
  ejecutarse a la vez; `rebuild --apply` necesita que esté detenido ([](#mantenimiento)).

Compatible:

- ext4, XFS y btrfs en almacenamiento local o de bloques: un disco, RAID, LVM, iSCSI o un volumen de
  bloques en la nube;
- ZFS con `sync=standard` (el predeterminado) o `sync=always`;
- NFSv4, o NFSv3 con su gestor de bloqueos, exportado con `sync`;
- un PersistentVolume de Kubernetes o un volumen de contenedor respaldado por uno de los anteriores.

No compatible:

- un montaje que desactive las barreras de escritura, o un disco virtual cuyo host ignore los
  vaciados;
- ZFS con `sync=disabled`, que reconoce `fsync` sin escribir;
- NFS exportado con `async`, que reconoce escrituras que el servidor NFS no ha almacenado, o montado
  con `nolock`, `local_lock=flock` o `local_lock=all`, que limitan los bloqueos a un solo host;
- recursos compartidos SMB/CIFS;
- sistemas de archivos FUSE (sshfs, rclone mount, s3fs, gcsfuse) y cualquier montaje de un
  almacenamiento de objetos;
- sistemas de archivos de agrupación y unión que reparten un árbol entre varios discos (mergerfs,
  recursos compartidos de usuario de Unraid en `/mnt/user`): un renombrado entre discos miembros es
  una copia, no un renombrado atómico;
- la capa escribible propia de un contenedor, y un directorio de un host macOS o Windows montado
  mediante bind en la máquina virtual de contenedores. Use un volumen dentro de la máquina virtual.

En Linux, `flock` sobre NFS se emula con bloqueos de rangos de bytes, que el gestor de bloqueos
comparte entre hosts. Con los bloqueos desactivados, `serve` y `purge --apply` en hosts distintos
pueden intercalar sus entradas en un libro de registro.

- **Haga una prueba de corte de corriente antes de confiar en el hardware.** `serve` comprueba al
  arrancar el tipo de sistema de archivos, el renombrado atómico y el uso compartido de bloqueos, pero
  ninguna comprobación de software puede detectar un disco, una controladora o un hipervisor que
  reconoce un vaciado que no ha hecho. En el hardware desplegado, suba archivos mientras corta la
  corriente (no un apagado limpio), luego arranque y ejecute `capsule-server scrub --deep`: cada
  subida que el servidor reconoció debe estar intacta
  ([](/design/filesystem/server/#blob-root-filesystem-contract)).
- **Deje espacio libre.** La raíz de blobs mantiene una reserva de espacio libre, de forma
  predeterminada el mayor valor entre el 2 % del volumen y 4 GiB. Por debajo de ella, las subidas
  nuevas se rechazan con `507` mientras las que ya están en el servidor terminan y las eliminaciones
  siguen funcionando ([](/design/quota/#enforcement-points)). La reserva está prevista pero aún no
  implementada; hasta entonces, un disco lleno rechaza todas las escrituras.
- **Depure la cuarentena de forma deliberada.** `quarantine/` conserva hasta que usted los elimine
  los blobs que no superaron un nuevo cálculo del hash o que no se pudieron leer, salvo los que se
  hayan reparado desde entonces. El futuro `capsule-server quarantine prune` muestra la lista antes de
  eliminar y no elimina nada sin `--apply`
  ([](/design/filesystem/server/#quarantine-retention)). `scrub` informa del tamaño de la
  cuarentena, así que vigílelo ahí.

<a id="reverse-proxy"></a>

### Proxy inverso

TLS termina en el proxy, nunca en el servidor
([](/design/networking/#transport-security)). El servidor escucha HTTP sin cifrar y solo debe ser
accesible desde el proxy: en loopback, en una red privada o en una red de pods. Configure el proxy
para que:

- acepte cuerpos de solicitud de al menos 16 MiB, el mayor fragmento de subida;
- transmita los cuerpos de solicitud al servidor en flujo en lugar de almacenarlos en búfer;
- desactive los datos tempranos de TLS (0-RTT), o los reenvíe con la cabecera `Early-Data: 1` para
  que el servidor pueda rechazar una escritura;
- no aplique un tiempo límite de duración total al cuerpo de una solicitud, y use un tiempo de
  inactividad mayor que el umbral de bloqueo del servidor, para que nunca se corte un fragmento lento
  pero constante;
- **deje pasar la compresión.** El servidor decide qué respuestas comprimir y nunca comprime
  `application/octet-stream` ([](/design/networking/#compression)). El proxy entrega las respuestas
  de Capsule tal como las envió el servidor: NO DEBE comprimir una respuesta con gzip ni de ninguna
  otra forma, NO DEBE descomprimirla ni añadir, cambiar o quitar `Content-Encoding`, y DEBE reenviar
  sin cambios el `Accept-Encoding` del cliente. En nginx, mantenga `gzip off;` para el sitio de
  Capsule, lo que anula un `gzip on;` heredado del `nginx.conf` de la distribución. En Caddy, no
  añada ninguna directiva `encode`; en Traefik, no asocie ningún middleware `compress`.

HTTP/2 y HTTP/3 en el lado del cliente del proxy son opcionales; los clientes recurren a HTTP/1.1.
TLS en el kernel (kTLS) y la descarga de TLS en la tarjeta de red, si los quiere, son ajustes del
proxy (en nginx, `ssl_conf_command Options KTLS;` con `sendfile on;`). La pila TLS propia del
servidor solo se usa para las conexiones que él abre: hacia un proveedor de OpenID Connect y hacia
pares federados. Estas confían en las raíces de Mozilla incluidas más cualquier `OIDC_CA_BUNDLE`, no
en el almacén de certificados del host.

<a id="clock"></a>

### Reloj

El servidor mide el tiempo con su propio reloj, y varias reglas dependen de que ese reloj sea
correcto. Manténgalo sincronizado con NTP (chrony o systemd-timesyncd) en el host. Un contenedor lee
el reloj del host, así que sincronice el host. El desfase DEBERÍA mantenerse dentro de un segundo, y
DEBE mantenerse dentro de 30 segundos. Más allá de esos límites:

| Desfase | Qué falla |
| --- | --- |
| Más de 30 s | Los códigos del segundo factor empiezan a fallar: un código solo se acepta dentro de un intervalo de 30 s respecto al reloj del servidor ([](/design/authentication/)) |
| Más de 5 min | La federación se detiene en ambas direcciones: los pares rechazan las solicitudes, indicaciones e informes firmados de este servidor, y este rechaza los suyos ([](/design/federation/)) |
| Reloj adelantado | La papelera se purga antes de que termine su plazo de retención, y se rechaza una restauración hecha a tiempo ([](/design/organization/#retention-window)). Las sesiones, los enlaces para compartir y las sesiones de subida caducan antes de tiempo |
| Reloj atrasado | Las sesiones, los enlaces para compartir y los contadores caducados siguen siendo válidos más tiempo del configurado |
| Desfase enorme | Los clientes rechazan los recibos de custodia cuyo `received_at` no es verosímil ([](/design/transfer/durability/)) |

Inicie `serve` solo cuando el reloj esté sincronizado. Con systemd, ordénelo después de
`time-sync.target` con `chrony-wait.service` o `systemd-time-wait-sync.service` habilitado. Una
máquina virtual restaurada desde una instantánea se reanuda con el reloj de la instantánea: deje que
NTP lo ajuste antes de que arranque `serve`. El servidor todavía no detecta ni informa por sí mismo
del desfase del reloj.

## Hardware

Estas cifras corresponden a un nodo que ejecuta el servidor y PostgreSQL juntos. Son una orientación
de dimensionamiento derivada de lo que hace el servidor, no pruebas de rendimiento.

El trabajo del servidor es ligero, así que la mayor parte del presupuesto de hardware debería ir al
**almacenamiento**. Calcula el hash SHA-256 de cada fragmento a medida que llega y lo vuelve a
calcular en la finalización y durante `scrub --deep`. Verifica la firma híbrida del cliente (Ed25519
y ML-DSA-65) en la finalización, firma los recibos de la misma manera y ejecuta Argon2id una vez por
cada inicio de sesión con contraseña (unos 19 MiB cada vez). Nunca decodifica, redimensiona ni
transcodifica contenido multimedia, nunca cifra ni descifra, y deja TLS al proxy inverso. El cuerpo
de una subida nunca se almacena en búfer más allá de un fragmento, así que la memoria no crece con el
tamaño del archivo.

| | Mínimo | Recomendado |
| --- | --- | --- |
| **CPU** | 2 núcleos, x86_64 o arm64 | 4 núcleos. Las instrucciones SHA-2 (SHA-NI en x86_64, las extensiones criptográficas de Armv8) aceleran el cálculo de hashes |
| **RAM** | 2 GB | De 4 a 8 GB. Más ayuda a la caché de PostgreSQL en una biblioteca grande, pero el almacenamiento compensa antes |
| **Almacenamiento del sistema** | SSD, 10 GB libres | SSD o NVMe para PostgreSQL |
| **Almacenamiento de blobs** | El tamaño de su biblioteca, más espacio para las subidas en curso | Discos redundantes (espejo, RAID o ZFS) dimensionados para varios años de crecimiento. Los discos mecánicos sirven |
| **Red** | 10 Mbit/s de subida | Gigabit en la LAN, y 50 Mbit/s o más de subida si los dispositivos sincronizan desde fuera de ella |
| **Alimentación** | — | Un SAI, para que un corte de corriente no interrumpa a PostgreSQL a mitad de una escritura |

- **`BLOB_ROOT` permanece en un único sistema de archivos.** El árbol completo, `incoming/` incluido,
  debe ser un único sistema de archivos, así que no puede dividirse entre un SSD y una cabina de
  discos ni entre dos datasets de ZFS. Póngalo todo en la cabina.
- **Dé a PostgreSQL la memoria que el servidor no usa.** Cada solicitud autenticada lee su sesión
  de PostgreSQL y cada fragmento de subida registra allí su rango, así que su caché sirve a la ruta
  de las solicitudes además de al índice de la biblioteca. Las sesiones, las sesiones de subida y los
  contadores son pequeños, y el servidor los elimina en cuanto caducan.
- **El ancho de banda de subida es el límite para los dispositivos remotos.** Un teléfono fuera de
  su red sube a la velocidad de *descarga* de su conexión, pero cada obtención de una vista previa o
  de un original sale a la velocidad de *subida*. Un fragmento solo se corta cuando deja de llegar,
  nunca por tardar mucho, y un cliente dimensiona sus fragmentos para terminar en un tiempo objetivo
  corto a la velocidad que mide, así que un enlace lento sube a su propio ritmo. Mantenga el umbral
  de bloqueo del servidor por debajo del tiempo de inactividad de su proxy, para que el servidor
  responda a un fragmento bloqueado antes de que el proxy lo corte.
- **Dimensione los discos para los originales, no para los derivados.** Los derivados son una
  fracción pequeña de una biblioteca de archivos RAW y vídeo. El servidor guarda cada blob una sola
  vez, tal como lo cifró el cliente.
- **Los usuarios añaden ancho de banda, no cálculo.** La búsqueda, las miniaturas y la organización
  se ejecutan en los clientes. Más personas navegando significa más bytes servidos, pero apenas más
  CPU o RAM.

`capsule-server/compose.yaml` ejecuta Postgres con la configuración adecuada para uso local
(`mise run serve-deps`). Sus credenciales son solo para desarrollo.

<a id="build"></a>

## Compilación

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

Los binarios enlazan glibc y no necesitan nada más instalado. Ejecútelos como un usuario sin
privilegios cuya única ruta con permiso de escritura sea `BLOB_ROOT`, la misma forma que impone la
imagen de contenedor que se describe a continuación.

<a id="container-image"></a>

## Imagen de contenedor

:::caution
Todavía no se ha publicado ninguna imagen, y las rutas `/livez` y `/readyz` y el subcomando
`healthcheck` aún no están implementados. Esta sección es el contrato según el cual se construye la
imagen.
:::

- **Base:** `gcr.io/distroless/cc-debian12:nonroot`, fijada por digest. Incluye glibc, el runtime
  de GCC, los certificados de CA y los datos de zonas horarias, y ningún shell ni gestor de paquetes.
- **Arquitecturas:** una única imagen multiarquitectura para `linux/amd64` y `linux/arm64`, cada una
  compilada de forma nativa para su destino glibc. Las compilaciones no se ajustan a una CPU
  concreta; el cálculo de hashes elige SHA-NI o las extensiones criptográficas de Armv8 en tiempo de
  ejecución.
- **Contenido:** `capsule-server` (el punto de entrada, `serve` de forma predeterminada) y
  `capsule-server-migration`, de la misma compilación.
- **Usuario:** sin privilegios de root, UID y GID 65532. El contenedor no necesita capacidades de
  Linux: elimínelas todas y active `no-new-privileges`.
- **Sistema de archivos raíz de solo lectura.** El servidor solo escribe en `BLOB_ROOT`, así que
  monte el directorio de blobs como el único volumen con permiso de escritura, propiedad del UID
  65532, y monte la raíz como de solo lectura.
- **Escucha:** defina `SERVER_HOST=0.0.0.0` (o `::`) dentro del contenedor; el puerto 3000 es el
  predeterminado. Publíquelo solo para el proxy inverso ([](#proxy-inverso)).
- **Los registros** van a stderr; defina `LOG_FORMAT=json` para un recolector de registros.
- **Parada:** `serve` se apaga de forma ordenada con `SIGTERM`. Una subida interrumpida por una
  parada se reanuda a partir de los rangos confirmados del servidor.
- **Una sola réplica.** Ejecute exactamente un `serve` por directorio de blobs
  ([](#directorio-de-blobs)). En Kubernetes, use una réplica con la estrategia de actualización
  `Recreate`: una actualización progresiva inicia el servidor nuevo mientras el antiguo sigue dando
  servicio.
- **Las migraciones** se ejecutan desde la misma etiqueta de imagen antes de que arranque `serve`,
  como un Job, un contenedor de inicialización o un `ExecStartPre=`:
  `capsule-server-migration up` ([](#migrar-y-luego-arrancar)).
- **Los comandos de mantenimiento** se ejecutan desde la misma imagen, con el mismo usuario y sobre
  el mismo volumen, como CronJobs o temporizadores ([](#mantenimiento)).
- **Estado:** `/livez` responde a un `GET` mientras el proceso da servicio y no comprueba nada más,
  para una sonda de actividad. `/readyz` solo responde mientras PostgreSQL responde, el esquema
  coincide con el binario y `BLOB_ROOT` admite escritura, para una sonda de disponibilidad. Ambas
  están fuera de `/v1`, así que una sonda no depende de la versión del protocolo, y ninguna necesita
  ser accesible a través del proxy. La imagen no incluye `curl`, así que su propia comprobación de
  estado ejecuta `capsule-server healthcheck`, que llama a `/livez` en el puerto de escucha local.
- **Reloj:** el contenedor lee el reloj del host; sincronice el host ([](#reloj)).

<a id="configure"></a>

## Configuración

La configuración se hace solo mediante el entorno. `capsule-server/.env.example` es una plantilla
de partida, y esta página nombra las variables que debe decidir quien opera el servidor. Un servidor
duradero se niega a arrancar (salida 2), indicando todas las variables que faltan a la vez, sin:

| Variable | Finalidad |
| --- | --- |
| `DATABASE_URL` | Conexión a PostgreSQL |
| `BLOB_ROOT` | Directorio de blobs |
| `JWT_ED25519_DER` | Clave Ed25519 en Base64 que firma los tokens de sesión |
| `ATTESTATION_KEY_SEED` | Semilla en Base64 que firma los recibos de custodia; debe ser independiente de la clave de tokens |

- **Proteja la clave de tokens por encima de todo.** La base de datos no contiene ningún secreto de
  portador, así que una base de datos o una copia de seguridad filtrada no proporciona por sí sola
  ninguna sesión; `JWT_ED25519_DER` es lo que convierte esa filtración en todas las sesiones activas
  ([](/design/authentication/#session-token)). La detección de reutilización de tokens de
  actualización ([](/design/authentication/#token-lineage-and-reuse)) no protege frente a ello,
  porque quien tiene la clave acuña tokens en lugar de reutilizarlos. Guárdela en su almacén de
  secretos, nunca en la base de datos ni en sus copias de seguridad, y rótela tras una sospecha de
  filtración, lo que cierra la sesión en todos los dispositivos.
- **Está prevista una clave de sellado para el segundo factor.** El servidor sellará en reposo los
  secretos TOTP con una clave dedicada, independiente de la clave de tokens, de modo que una
  filtración de la base de datos o de una copia de seguridad no proporcione ningún segundo factor y
  rotar la clave de tokens no deje ninguno inservible ([](/design/authentication/#the-second-factor)).
  Su ajuste aún no existe; cuando llegue, manténgala separada de la base de datos y de sus copias de
  seguridad, igual que la clave de tokens.
- **El registro abierto necesita una cuota predeterminada** (previsto). Cuando las cuotas sean
  configurables, `serve` se negará a arrancar con el registro por contraseña activado y sin un
  límite predeterminado, y quien no quiera ninguno deberá establecerlo explícitamente en
  `unlimited`, lo que se registra con nivel `WARN` en cada arranque
  ([](/design/quota/#thresholds-and-states)). Hoy todas las instalaciones funcionan sin límite.

`SYNC_CURSOR_MAC_KEY` se deriva de `JWT_ED25519_DER` cuando no está definida, así que rotar la clave
de tokens también invalida los cursores de sincronización pendientes, a menos que la defina
explícitamente. La federación está desactivada salvo que se definan `FEDERATION_ENABLED=true` y
`FEDERATION_URL`; déjela desactivada.

Indique su proxy inverso para que los límites de frecuencia por origen vean al cliente y no al proxy.

| Variable | Significado |
| --- | --- |
| `TRUSTED_PROXIES` | Direcciones y redes CIDR separadas por comas (`10.0.0.0/8`). Una red `/0` se rechaza, porque se fiaría de cualquier remitente. |
| `TRUSTED_PROXY_HOPS` | Confía en ese número de saltos de reenvío más cercanos al servidor, sea cual sea su dirección. |
| `TRUSTED_PROXY_HEADER` | El único campo de reenvío que lee el servidor: `x-forwarded-for` (el predeterminado) o `forwarded` (RFC 7239). Con cualquier otro valor, el servidor no arranca. |

Si no están definidas, no se confía en ninguna cabecera de reenvío. Es seguro, pero entonces todos
los clientes detrás del proxy comparten el cupo del proxy, de modo que un desconocido que intente
adivinar contraseñas ralentiza el inicio de sesión de todos
([](/design/authentication/#sign-in-limits)); el servidor registra una advertencia cuando ve una
cabecera de reenvío sin confiar en ningún salto. El error contrario es peor: confiar en un salto que
no añade la dirección del cliente permite que cada cliente elija su propia clave de límite de
frecuencia. El campo que `TRUSTED_PROXY_HEADER` no nombra se ignora, lo envíe quien lo envíe, así
que un cliente no puede elegir su propia clave de límite de frecuencia enviando uno que su proxy deje
pasar. Indique el campo al que su proxy añade la dirección. Con el valor predeterminado, haga que su
proxy añada a `X-Forwarded-For` la dirección que vio, como hace nginx con:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Defina `TRUSTED_PROXY_HEADER=forwarded` solo si su proxy añade su propio elemento a `Forwarded` y no
escribe `X-Forwarded-For`.

<a id="migrate-then-start"></a>

## Migrar y luego arrancar

`serve` nunca migra y rechaza un esquema de base de datos más antiguo que el binario. Ejecute
primero las migraciones, en cada actualización:

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` escribe los registros en stderr y muestra `listening on <url>` en stdout. No use `--memory`
en producción: mantiene todo salvo los blobs en la memoria del proceso.

<a id="maintenance"></a>

## Mantenimiento

Programe estos comandos de ejecución única (cron o un temporizador). Necesitan `DATABASE_URL` y
`--blob-root`, y de forma predeterminada hacen una simulación:

- `capsule-server gc --apply`: recupera los blobs sin referencias tras un periodo de gracia y elimina
  las sesiones, las sesiones de subida y los contadores caducados. Ejecútelo con regularidad aunque
  el almacenamiento no escasee.
- `capsule-server purge --apply`: purga definitivamente los recursos eliminados de forma lógica cuyo
  plazo de retención ha vencido. También necesita el `ATTESTATION_KEY_SEED` del servidor, porque
  cada purga se firma primero en el libro de registro del propietario
  ([](/design/filesystem/server/#the-owner-ledger)), y se niega a ejecutarse sin ella. Termina con
  un código distinto de cero cuando el libro de registro no pudo registrar una purga; ese recurso se
  conserva para la siguiente ejecución.
- `capsule-server scrub [--deep]`: comprobación de integridad de solo lectura del índice frente al
  almacén de blobs. **Termina con un código distinto de cero ante cualquier hallazgo**, así que
  configure una alerta para ello ([](/design/filesystem/maintenance/#server-side-integrity-scrub)).

Después de restaurar una copia de seguridad o de perder la base de datos, reconstruya el índice a
partir de los libros de registro de los propietarios en `BLOB_ROOT`
([](/design/filesystem/server/#rebuild)). Defina `ATTESTATION_KEY_SEED` en el host para que verifique
las firmas de los libros de registro.

1. `capsule-server rebuild` compara el índice con los libros de registro.
2. Con `serve` detenido, `capsule-server rebuild --apply` restaura las filas que faltan en el
   índice. Nunca sobrescribe una fila que el índice ya tiene. Vuelve a anclar un libro de registro
   que perdió su final, tras lo cual el servidor vuelve a escribir para ese propietario.
3. Si el informe dice que se reescribió un libro de registro, el servidor rechaza las escrituras de
   ese propietario hasta que usted lo investigue y ejecute
   `capsule-server rebuild --apply --accept-rewrites`.

Un servidor que aplicó operaciones de ciclo de vida (eliminar, restaurar, actualizar metadatos,
derivado, reemplazar) antes de registrar el tamaño de sus blobs guarda esas referencias con tamaño
0. Ejecute `capsule-server repair-sizes` una vez para listarlas y luego
`capsule-server repair-sizes --apply` para completar cada tamaño a partir del almacén de blobs. Es
seguro ejecutarlo junto a un `serve` en marcha, y cada recurso reparado se vuelve a enviar a los
clientes. Termina con un código distinto de cero cuando falta un blob o está vacío, y deja esa
referencia como estaba.

<a id="backups"></a>

## Copias de seguridad

El estado duradero es PostgreSQL más `BLOB_ROOT` (los blobs y `ledger/`); haga copia de ambos,
juntos. La base de datos también contiene las sesiones iniciadas y las sesiones de subida en curso,
así que una restauración mantiene la sesión de los usuarios. Guarde también en su almacén de
secretos las variables de clave anteriores: no están en la base de datos, y la identidad de firma y
de atestación del servidor depende de ellas.

Copie primero la base de datos y luego los blobs:

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` puede seguir en marcha. Un blob está en disco y su entrada añadida al libro de registro antes
de que se confirme la fila del índice que lo nombra, así que una copia de los blobs hecha después del
volcado contiene todos los blobs a los que hace referencia el volcado, y posiblemente escrituras que
el volcado no recogió. No ejecute `gc --apply` ni `purge --apply` mientras se hace la copia de
seguridad: cualquiera de los dos puede eliminar un blob al que el volcado todavía hace referencia.

La reconstrucción que se describe a continuación restaura los recursos subidos entre el volcado y la
copia de los blobs, porque añade todas las filas que faltan en el índice. Nunca cambia una fila que
el volcado ya contiene, así que una eliminación, una restauración o una edición de metadatos hecha en
ese intervalo sobre un recurso más antiguo no se aplica. Una reconstrucción informa de ese cambio
como una fila que difiere (y termina con un código distinto de cero) solo cuando cambió los blobs del
recurso; una eliminación o una restauración pasa desapercibida. Cuando una copia de seguridad deba
recoger todos los cambios, detenga `serve` mientras toma ambas mitades. Si una restauración informa
de filas que difieren y el host antiguo sigue en marcha, vuelva a hacer la copia de esa manera.

<a id="restoring-onto-a-new-host"></a>

### Restaurar en un host nuevo

1. Inicie un PostgreSQL vacío y defina los mismos `JWT_ED25519_DER` y `ATTESTATION_KEY_SEED` que en
   el host antiguo.
2. Restaure la base de datos. El volcado incluye el esquema y su registro de migraciones, así que no
   ejecute antes `capsule-server-migration up`:
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Copie el directorio de blobs de vuelta a `BLOB_ROOT`, en un único sistema de archivos.
4. Con `serve` detenido, ejecute `capsule-server rebuild` y lea su informe. Enumera los recursos
   subidos después del volcado; `capsule-server rebuild --apply` los restaura con los números de
   secuencia que tenían, así que los cursores de sincronización de los clientes siguen siendo
   válidos.
5. Ejecute `capsule-server scrub --deep`. No debería informar de nada; termina con un código
   distinto de cero ante cualquier hallazgo.
6. Inicie `serve`. Las sesiones vuelven con la base de datos, así que un cliente con la sesión
   iniciada en el momento del volcado sigue con ella, salvo que su sesión haya caducado desde
   entonces. Antes de empezar a escuchar, `serve` descarta todas las sesiones de subida sin terminar
   cuyo archivo provisional en `BLOB_ROOT` no se restauró, y elimina todos los archivos provisionales
   sin sesión; un cliente cuya subida se descartó vuelve a empezar ese archivo. Los clientes reanudan
   la sincronización donde la dejaron.

Si se pierde la base de datos y no hay volcado, migre una base de datos vacía y ejecute
`capsule-server rebuild --apply` sobre el directorio de blobs. Restaura todas las filas de recursos a
partir de los libros de registro de los propietarios. Las cuentas, los directorios de dispositivos
publicados, la pertenencia a álbumes y los enlaces para compartir solo están en la base de datos, y
una reconstrucción no los restaura.

Ensaye este proceso antes de confiarle datos reales, por ejemplo comprobando con `capsule restore`
que cada original vuelve byte por byte. `capsule-e2e/tests/backup_restore_drill.rs` ejecuta los
mismos pasos contra contenedores (`mise run test-rust-containers`).
