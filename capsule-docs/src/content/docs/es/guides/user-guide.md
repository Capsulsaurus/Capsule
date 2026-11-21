---
title: Guía de uso
description: El uso diario de una biblioteca de Capsule con la CLI capsule — importar, hacer copias de seguridad, verificar, organizar y eliminar con seguridad.
status: draft
---

Esta guía cubre la CLI `capsule`, el cliente de referencia. Los comandos marcados como *(previsto)*
están en la [hoja de ruta](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md) y aún no
están disponibles. `capsule <command> --help` es la referencia autorizada para las opciones; la
referencia de la CLI ([](/reference/cli/)) las enumera todas.

:::caution
Capsule está en fase alfa. Hasta la primera versión de uso interno, conserve una copia de seguridad
independiente de cualquier original que importe.
:::

<a id="set-up"></a>

## Configuración

1. Cree una biblioteca: `capsule library init --name "Photos"`. La biblioteca es un directorio
   local cifrado; su frase de contraseña protege las claves en este equipo.
2. Inicie sesión en su servidor: `capsule auth register` o `capsule auth login` (`--oidc` o
   `--device` para el inicio de sesión único).
3. Configure la recuperación: `capsule auth escrow`. Guarde sin conexión el secreto de recuperación
   que se muestra. Sin él y sin su frase de contraseña, perder el equipo significa perder la
   biblioteca, porque el servidor no puede descifrar nada.

<a id="import-and-back-up"></a>

## Importar y hacer copias de seguridad

- `capsule import <path>` copia archivos en la biblioteca, firma la procedencia y los metadatos de
  cada recurso y genera miniaturas. `--provider takeout` importa un archivo de Google Takeout con
  sus metadatos sidecar ([](/guides/google-photos-migration/)).
- `capsule push` sube la biblioteca. Las subidas se reanudan donde se detuvieron, así que volver a
  ejecutar `push` siempre es seguro. Después de cada recurso, `push` pide al servidor que confirme
  que conserva de forma duradera cada blob y guarda en la biblioteca los recibos de custodia firmados
  del servidor. Un recurso que falla se notifica y la ejecución continúa. El comando termina con un
  código distinto de cero si ha fallado algún recurso, y `--json` enumera el resultado de cada uno.
- `capsule import --push` hace ambas cosas en un solo paso.
- **Conserve sus originales hasta que se verifique la custodia.** Un recurso solo está almacenado
  de forma segura cuando el servidor informa de que es duradero y ha emitido un recibo de custodia
  firmado, que `push` notifica como verificado. No elimine los archivos de origen antes.
  `import --move --push` elimina cada origen solo después de que su recurso se haya verificado. Sin
  `--push`, `--move` conserva todos los orígenes y lo indica, y un `import --move --push` posterior
  sobre los mismos archivos los libera.
- `capsule restore` *(previsto)* descarga los originales y los compara byte por byte con lo que se
  firmó en la importación.

<a id="organize"></a>

## Organizar

- Las **etiquetas** son de formato libre y se asocian a un recurso, una pila (par RAW+JPEG, ráfaga,
  horquillado), un álbum o una región de una imagen. Las etiquetas que añade se mantienen separadas
  de las sugeridas por la IA, y puede aceptar o descartar una sugerencia. Las etiquetas de recursos
  ya existen; las de pilas, álbumes y regiones están *(previstas)*.
- `capsule show <asset>` muestra lo que registran los metadatos firmados de un recurso: pie de foto,
  valoración, etiquetas, ubicación y hora de captura.
- `capsule repair capture-time` encuentra los recursos cuya hora de captura no coincide con el EXIF
  del original y, con `--apply`, la corrige. La edición de la ubicación está *(prevista)*.

Cada edición es un cambio firmado y de solo anexado: el valor anterior permanece en el historial de
procedencia del recurso, y las ediciones simultáneas desde dos dispositivos se fusionan de forma
determinista.

<a id="delete-safely"></a>

## Eliminar con seguridad

Corrija metadatos como la geolocalización antes de eliminar, mientras los recursos de alrededor
todavía le dan contexto. Una foto sin ubicación propia es fácil de situar mientras sus vecinas del
mismo paseo están en la biblioteca; cuando desaparecen, ese contexto desaparece con ellas.

- `capsule cull` marca los recursos como elegido, neutro o descartado, y filtra por marca.
- `capsule cull --sweep` mueve los descartados a la papelera. Los recursos en la papelera siguen
  siendo recuperables durante el plazo de retención (`--retain-days`) antes de purgarse.
- `capsule trash delete --asset <id>` mueve un recurso a la papelera tanto en el servidor como en
  la biblioteca. Un recurso que ya está en la papelera, por ejemplo después de
  `capsule cull --sweep`, conserva el plazo de retención con el que se envió a la papelera: `--retain-days` solo
  se aplica a una eliminación nueva, y `--json` informa del plazo enviado como `retention_until`.
- `capsule trash restore --asset <id>` recupera un recurso. Una restauración después de que haya
  vencido el plazo de retención se rechaza (`retention_elapsed` con `--json`) y no se reintenta; el
  recurso permanece en la papelera hasta que se purga.

<a id="share"></a>

## Compartir

- `capsule share create --album <id>` o `--asset <id>` emite un enlace de solo lectura;
  `--protect` añade una frase de contraseña al enlace y `--expires-at` una caducidad. El enlace
  lleva su clave en el fragmento de la URL, que nunca llega al servidor.
- `capsule share revoke <id>` impide que un enlace se abra.

<a id="sync-another-machine"></a>

## Sincronizar otro equipo

- `capsule sync` descarga del servidor el registro de cambios de la biblioteca; `capsule list`
  muestra lo que ha recibido.
- `capsule auth recover` lleva la cuenta a un equipo nuevo a partir del secreto de recuperación.
