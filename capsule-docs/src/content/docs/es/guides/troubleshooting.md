---
title: Solución de problemas
description: Recopile la versión y los registros que necesita un informe, y resuelva los problemas habituales con un servidor de Capsule, una importación o una biblioteca.
status: draft
---

Empiece aquí cuando algo vaya mal. Si nada de lo que sigue lo resuelve, [](/support/) indica dónde
preguntar, y los diagnósticos de la primera sección son lo que necesita un informe de error.

<a id="collect-diagnostics"></a>

## Recopilar diagnósticos

<a id="version"></a>

### Versión

No se ha publicado ninguna versión, así que el número de versión no distingue unas compilaciones de
otras. Todo se compila desde el código fuente, así que indique el commit a partir del cual compiló:
`git rev-parse --short HEAD` en su copia del repositorio. La CLI no tiene una opción de versión, y
`capsule-server --version` solo muestra la versión del crate, que comparten todas las compilaciones.

<a id="logs"></a>

### Registros

La CLI y el servidor escriben los registros en **stderr**. Stdout solo contiene lo que muestra un
comando, y con `--json` eso es un único documento JSON, así que puede capturar cada uno por separado:

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` define cuánto se registra. Una compilación de versión registra en `info` de forma
  predeterminada y una de depuración en `debug`. Use `RUST_LOG=debug` cuando reproduzca un problema,
  y `RUST_LOG=trace` solo cuando se le pida: genera muchísimo.
- El formato de registro del servidor se define con `LOG_FORMAT`: `pretty` de forma predeterminada
  en las compilaciones de depuración y `json` en las de versión
  ([](/development/local-development/#logs)).
- La TUI a pantalla completa (un `capsule` sin argumentos en una terminal) no puede registrar en la
  pantalla que dibuja, así que escribe `tui.log` en el directorio de caché de la CLI, que
  `capsule status` muestra como su Cache Directory. El archivo se sobrescribe en cada sesión, así que
  cópielo antes de volver a iniciar la TUI.
- La salida de `--json` es el registro más preciso de lo que hizo un comando, porque informa del
  resultado de cada recurso. Adjúntela cuando un comando informe de fallos.

**Elimine los datos sensibles antes de compartir.** Quite frases de contraseña, secretos de
recuperación, tokens, claves, direcciones de servidor y rutas de archivos personales. Nunca pegue
la frase de contraseña de una biblioteca ni un secreto de recuperación en ningún sitio, ni siquiera
en un informe privado.

<a id="the-server"></a>

## El servidor

<a id="it-refuses-to-start"></a>

### No arranca

- **Estado de salida 2, con nombres de variables.** Un servidor duradero necesita `DATABASE_URL`,
  `BLOB_ROOT`, `JWT_ED25519_DER` y `ATTESTATION_KEY_SEED`, e indica todas las que faltan a la vez.
  Consulte [](/guides/self-hosting/#configure).
- **El esquema de la base de datos es más antiguo que el binario.** `serve` nunca migra. Ejecute
  `capsule-server-migration up` en cada actualización, antes de `serve`
  ([](/guides/self-hosting/#migrate-then-start)).
- **Se rechaza una configuración de proxy de confianza.** `TRUSTED_PROXIES` rechaza una red `/0`, y
  `TRUSTED_PROXY_HEADER` solo acepta `x-forwarded-for` o `forwarded`.

Un servidor en ejecución muestra `listening on <url>` en stdout.

<a id="clients-cant-sign-in-or-connect"></a>

### Los clientes no pueden iniciar sesión ni conectarse

- `capsule server-info` muestra cómo inicia sesión la gente en el servidor: si tiene un proveedor de
  identidad y si rechaza las contraseñas. Un inicio de sesión con contraseña en un servidor que tiene
  las contraseñas desactivadas falla por diseño.
- Detrás de un proxy inverso, todos los clientes comparten el límite de frecuencia del proxy hasta
  que el proxy figura en `TRUSTED_PROXIES` o `TRUSTED_PROXY_HOPS`. Cuando varias personas alcanzan
  el límite a la vez, compruebe esto primero ([](/guides/self-hosting/#configure)).

<a id="the-index-and-the-blob-store-disagree"></a>

### El índice y el almacén de blobs no coinciden

`capsule-server scrub` es una comprobación de integridad de solo lectura, y termina con un código
distinto de cero ante cualquier hallazgo. Después de restaurar una copia de seguridad o de perder la
base de datos, `capsule-server rebuild` compara el índice con los libros de registro de los propietarios. No
cambia nada hasta que detenga `serve` y ejecute `capsule-server rebuild --apply`
([](/guides/self-hosting/#maintenance)).

<a id="imports-and-uploads"></a>

## Importaciones y subidas

- **Un `push` se detuvo a medias.** Vuelva a ejecutarlo. Las subidas se reanudan donde se
  detuvieron, así que volver a ejecutarlo siempre es seguro. `push` termina con un código distinto de
  cero cuando ha fallado algún recurso, y `--json` enumera el resultado de cada uno
  ([](/guides/user-guide/#import-and-back-up)).
- **Conserve sus originales hasta que se verifique la custodia.** No elimine un archivo de origen
  hasta que `push` informe de que su recurso está verificado, o use `import --move --push`, que
  elimina cada origen solo después de que su recurso se haya verificado.
- **Una exportación de Google Fotos parece incompleta.** Compruebe que se descargaron y extrajeron
  todas las partes de la exportación de Takeout, y lea lo que el importador no puede garantizar
  ([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **Las horas de captura son incorrectas.** `capsule repair capture-time` informa de cada recurso
  cuya hora de captura firmada no coincide con el EXIF del original, y `--apply` las corrige. Cada
  corrección es un registro firmado permanente, así que ejecútelo primero sin `--apply`.

<a id="check-a-librarys-integrity"></a>

## Comprobar la integridad de una biblioteca

Si parece que faltan fotos o que están dañadas, **deténgase y no elimine nada** mientras lo
averigua: eliminar puede quitar el contexto que necesita para recuperarlas.

1. `capsule verify --library <path>` pregunta al servidor si conserva de forma duradera cada
   original. `--deep` hace que el servidor vuelva a calcular el hash de los bytes almacenados en
   lugar de comprobar solo que están presentes, y `--signed` guarda en la biblioteca el veredicto
   firmado del servidor.
2. Los recursos en la papelera siguen siendo recuperables durante su plazo de retención:
   `capsule trash restore --library <path> --asset <id>` recupera uno
   ([](/guides/user-guide/#delete-safely)).
3. La pérdida de un dispositivo o de una clave tiene sus propios procedimientos:
   [](/design/backup-recovery/#recovery-paths).

Si se han perdido o dañado datos, infórmelo como un error con la salida de cada paso. Perder o
dañar datos es un defecto, sea cual sea su causa.

<a id="still-stuck"></a>

## ¿Sigue atascado?

[](/support/) indica dónde hacer una pregunta y cómo presentar un informe de error sobre el que se
pueda actuar.
