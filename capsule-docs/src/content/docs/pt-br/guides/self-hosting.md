---
title: Auto-hospedagem
description: Como rodar o capsule-server sobre PostgreSQL e um armazenamento de blobs no sistema de arquivos
audit: none
---

Nenhuma versão foi lançada ainda, então você compila o servidor a partir do código-fonte. O servidor
armazena apenas texto cifrado e nunca decodifica mídia; os clientes fazem todo o processamento e a
criptografia.

<a id="what-you-need"></a>

## Do que você precisa

- **PostgreSQL 18**: o índice durável (contas, itens, cota) e o estado temporário do servidor
  (sessões de login, sessões de envio, contadores de taxa). É o único banco de dados que o servidor
  usa, e ele precisa rodar com as configurações de durabilidade intactas ([](#postgresql)).
- **Um diretório de blobs** (`BLOB_ROOT`) em **um único sistema de arquivos POSIX** que respeite
  `fsync` e compartilhe bloqueios de arquivo. Ele guarda os blobs e, em `ledger/`, o registro
  assinado de cada proprietário, a partir do qual o índice pode ser reconstruído
  ([](#diretório-de-blobs)).
- **Um proxy reverso que termine o TLS** na frente do servidor e que repasse a compressão sem
  mexer nela ([](#proxy-reverso)).
- **Um relógio sincronizado** no host, mantido por NTP ([](#relógio)).

### PostgreSQL

O servidor responde a uma escrita assim que a linha correspondente é confirmada, então um commit que
o PostgreSQL reconhece e depois perde pode trazer de volta uma sessão revogada ou aceitar duas vezes
um código do segundo fator. Mantenha:

- `fsync = on` e `full_page_writes = on`, os valores padrão;
- `synchronous_commit` em `on`, `local`, `remote_write` ou `remote_apply`, nunca `off`;
- o diretório de dados em um armazenamento que respeite o flush. Um cache de escrita de disco sem
  proteção contra queda de energia, ou um disco virtual cujo host ignora flushes, perde commits já
  reconhecidos em uma queda de energia ([](/design/filesystem/server/)).

O PostgreSQL pode ficar em um sistema de arquivos, disco ou host diferente do `BLOB_ROOT`.

<a id="blob-directory"></a>

### Diretório de blobs

`BLOB_ROOT` é um contrato com o sistema de arquivos
([](/design/filesystem/server/#blob-root-filesystem-contract)), organizado como em
[](/design/filesystem/server/#blob-store-layout):

- **Um único sistema de arquivos.** A finalização é uma renomeação atômica, então a árvore inteira,
  incluindo `incoming/`, não pode se estender por vários volumes, datasets ou PersistentVolumes do
  Kubernetes.
- **`fsync` é respeitado**, em arquivos e em diretórios, antes de o servidor reconhecer uma escrita.
- **Os bloqueios de arquivo são compartilhados** por todos os processos que escrevem na árvore.
  `serve` e `purge --apply` bloqueiam o `ledger/<owner>/.lock` de cada proprietário antes de
  acrescentar entradas ([](/design/filesystem/server/#concurrent-appenders)).
- **Um `serve` por diretório de blobs.** Os blocos de envio e a finalização são reivindicados dentro
  do processo que atende ([](/design/transfer/upload/)), então dois processos `serve` na mesma
  árvore, no mesmo host ou em dois, não são suportados. `gc`, `purge` e `scrub` podem rodar ao lado
  dele; `rebuild --apply` exige que ele esteja parado ([](#manutenção)).

Suportados:

- ext4, XFS e btrfs em armazenamento local ou em bloco: um disco, RAID, LVM, iSCSI ou um volume de
  bloco na nuvem;
- ZFS com `sync=standard` (o padrão) ou `sync=always`;
- NFSv4, ou NFSv3 com o gerenciador de bloqueios, exportado com `sync`;
- um PersistentVolume do Kubernetes ou um volume de contêiner baseado em uma das opções acima.

Não suportados:

- uma montagem que desativa as barreiras de escrita, ou um disco virtual cujo host ignora flushes;
- ZFS com `sync=disabled`, que reconhece o `fsync` sem gravar;
- NFS exportado com `async`, que reconhece escritas que o servidor NFS não armazenou, ou montado com
  `nolock`, `local_lock=flock` ou `local_lock=all`, que restringem os bloqueios a um único host;
- compartilhamentos SMB/CIFS;
- sistemas de arquivos FUSE (sshfs, rclone mount, s3fs, gcsfuse) e qualquer montagem de
  armazenamento de objetos;
- sistemas de arquivos de pool e de união que espalham uma árvore por vários discos (mergerfs,
  compartilhamentos de usuário do Unraid em `/mnt/user`): uma renomeação entre discos membros é uma
  cópia, não uma renomeação atômica;
- a camada gravável do próprio contêiner, e um diretório de um host macOS ou Windows montado via
  bind na VM de contêineres. Use um volume dentro da VM.

No Linux, o `flock` sobre NFS é emulado com bloqueios de intervalo de bytes, que o gerenciador de
bloqueios compartilha entre hosts. Com os bloqueios desativados, `serve` e `purge --apply` em hosts
diferentes podem intercalar suas entradas em um registro.

- **Faça um teste de queda de energia antes de confiar no hardware.** `serve` verifica na
  inicialização o tipo de sistema de arquivos, a renomeação atômica e o compartilhamento de
  bloqueios, mas nenhuma verificação de software enxerga um disco, controladora ou hipervisor que
  reconhece um flush que não fez. No hardware de produção, envie arquivos enquanto corta a energia
  (não um desligamento normal), depois inicie e execute `capsule-server scrub --deep`: todo envio
  que o servidor reconheceu precisa estar intacto
  ([](/design/filesystem/server/#blob-root-filesystem-contract)).
- **Deixe espaço livre.** A raiz de blobs mantém uma reserva de espaço livre, por padrão o maior
  valor entre 2% do volume e 4 GiB. Abaixo dela, novos envios são recusados com `507`, enquanto os
  envios que já estão no servidor terminam e as exclusões continuam funcionando
  ([](/design/quota/#enforcement-points)). A reserva está planejada e ainda não foi implementada;
  até lá, um disco cheio recusa todas as escritas.
- **Limpe a quarentena de forma deliberada.** `quarantine/` guarda os blobs que falharam em um novo
  cálculo de hash ou que não puderam ser lidos até você excluí-los, exceto os que foram reparados
  desde então. O futuro `capsule-server quarantine prune` lista antes de remover e não remove nada
  sem `--apply` ([](/design/filesystem/server/#quarantine-retention)). `scrub` informa o tamanho da
  quarentena, então acompanhe por ali.

<a id="reverse-proxy"></a>

### Proxy reverso

O TLS termina no proxy, nunca no servidor ([](/design/networking/#transport-security)). O servidor
escuta HTTP puro e precisa estar acessível somente a partir do proxy: em loopback, em uma rede
privada ou em uma rede de pods. Configure o proxy para:

- aceitar corpos de requisição de pelo menos 16 MiB, o maior bloco de envio;
- transmitir os corpos de requisição ao servidor em streaming, em vez de colocá-los em buffer;
- desativar os dados antecipados do TLS (0-RTT), ou repassá-los com o cabeçalho `Early-Data: 1`
  para que o servidor possa recusar uma escrita;
- não aplicar nenhum tempo limite de duração total ao corpo de uma requisição, e usar um tempo
  limite de inatividade maior que o limite de travamento do servidor, para que um bloco lento, mas
  constante, nunca seja cortado;
- **repassar a compressão.** O servidor decide quais respostas comprimir e nunca comprime
  `application/octet-stream` ([](/design/networking/#compression)). O proxy repassa as respostas do
  Capsule exatamente como o servidor as enviou: ele NÃO DEVE comprimir uma resposta com gzip nem de
  outra forma, NÃO DEVE descomprimi-la nem adicionar, alterar ou remover `Content-Encoding`, e DEVE
  repassar inalterado o `Accept-Encoding` do cliente. No nginx, mantenha `gzip off;` para o site do
  Capsule, o que sobrepõe um `gzip on;` herdado do `nginx.conf` da distribuição. No Caddy, não
  adicione nenhuma diretiva `encode`; no Traefik, não associe nenhum middleware `compress`.

HTTP/2 e HTTP/3 no lado do cliente do proxy são opcionais; os clientes recorrem ao HTTP/1.1. TLS no
kernel (kTLS) e offload de TLS na placa de rede, se você quiser, são configurações do proxy (no
nginx, `ssl_conf_command Options KTLS;` com `sendfile on;`). A pilha TLS do próprio servidor só é
usada nas conexões que ele abre: com um provedor OpenID Connect e com pares federados. Essas
conexões confiam nas raízes da Mozilla embutidas mais qualquer `OIDC_CA_BUNDLE`, e não no
repositório de certificados do host.

<a id="clock"></a>

### Relógio

O servidor mede o tempo pelo próprio relógio, e várias regras dependem de esse relógio estar certo.
Mantenha-o sincronizado com NTP (chrony ou systemd-timesyncd) no host. Um contêiner lê o relógio do
host, então sincronize o host. A diferença DEVERIA ficar dentro de um segundo, e DEVE ficar dentro
de 30 segundos. Além desses limites:

| Diferença | O que quebra |
| --- | --- |
| Mais de 30 s | Os códigos do segundo fator começam a falhar: um código só é aceito dentro de um intervalo de 30 s em relação ao relógio do servidor ([](/design/authentication/)) |
| Mais de 5 min | A federação para nas duas direções: os pares recusam as requisições, dicas e relatórios assinados deste servidor, e ele recusa os deles ([](/design/federation/)) |
| Relógio adiantado | A lixeira é esvaziada antes do fim do prazo de retenção, e uma restauração feita a tempo é recusada ([](/design/organization/#retention-window)). Sessões, links de compartilhamento e sessões de envio expiram antes da hora |
| Relógio atrasado | Sessões, links de compartilhamento e contadores expirados continuam válidos por mais tempo que o configurado |
| Diferença grosseira | Os clientes recusam recibos de custódia cujo `received_at` não é plausível ([](/design/transfer/durability/)) |

Só inicie o `serve` depois que o relógio estiver sincronizado. Com systemd, ordene-o depois de
`time-sync.target` com `chrony-wait.service` ou `systemd-time-wait-sync.service` habilitado. Uma
máquina virtual restaurada de um snapshot retoma com o relógio do snapshot: deixe o NTP ajustá-lo
antes de o `serve` iniciar. O servidor ainda não detecta nem informa por conta própria a diferença
de relógio.

## Hardware

Estes números valem para um nó que roda o servidor e o PostgreSQL juntos. São orientações de
dimensionamento derivadas do que o servidor faz, não benchmarks.

O trabalho do servidor é leve, então a maior parte do orçamento de hardware deve ir para o
**armazenamento**. Ele calcula o hash SHA-256 de cada bloco à medida que chega e recalcula na
finalização e durante o `scrub --deep`. Ele verifica a assinatura híbrida do cliente (Ed25519 e
ML-DSA-65) na finalização, assina os recibos do mesmo jeito e executa o Argon2id uma vez por login
com senha (cerca de 19 MiB cada). Ele nunca decodifica, redimensiona ou transcodifica mídia, nunca
criptografa nem descriptografa, e deixa o TLS para o proxy reverso. O corpo de um envio nunca fica
em buffer além de um bloco, então a memória não cresce com o tamanho do arquivo.

| | Mínimo | Recomendado |
| --- | --- | --- |
| **CPU** | 2 núcleos, x86_64 ou arm64 | 4 núcleos. As instruções SHA-2 (SHA-NI no x86_64, as extensões de criptografia do Armv8) aceleram o cálculo de hashes |
| **RAM** | 2 GB | De 4 a 8 GB. Mais memória ajuda o cache do PostgreSQL em uma biblioteca grande, mas o armazenamento dá retorno primeiro |
| **Armazenamento do sistema** | SSD, 10 GB livres | SSD ou NVMe para o PostgreSQL |
| **Armazenamento de blobs** | O tamanho da sua biblioteca, mais espaço para os envios em andamento | Discos redundantes (espelho, RAID ou ZFS) dimensionados para vários anos de crescimento. Discos rígidos convencionais servem |
| **Rede** | 10 Mbit/s de upload | Gigabit na LAN, e 50 Mbit/s ou mais de upload se os dispositivos sincronizarem de fora dela |
| **Energia** | — | Um nobreak, para que uma queda de energia não interrompa o PostgreSQL no meio de uma escrita |

- **O `BLOB_ROOT` fica em um único sistema de arquivos.** A árvore inteira, incluindo `incoming/`,
  precisa ser um só sistema de arquivos, então não pode ser dividida entre um SSD e um array de
  discos nem entre dois datasets ZFS. Coloque tudo no array.
- **Dê ao PostgreSQL a memória que o servidor não usa.** Toda requisição autenticada lê a sessão no
  PostgreSQL e todo bloco de envio registra ali o seu intervalo, então o cache dele atende o caminho
  das requisições além do índice da biblioteca. Sessões, sessões de envio e contadores são pequenos,
  e o servidor os exclui assim que expiram.
- **A banda de upload é o limite para dispositivos remotos.** Um celular fora da sua rede envia na
  velocidade de *download* da sua conexão, mas cada busca de uma prévia ou de um original sai na sua
  velocidade de *upload*. Um bloco só é cortado quando para de chegar, nunca por demorar, e o
  cliente dimensiona seus blocos para terminarem em um tempo-alvo curto na velocidade que mede,
  então um link lento envia no próprio ritmo. Mantenha o limite de travamento do servidor abaixo do
  tempo limite de inatividade do proxy, para que o servidor responda a um bloco travado antes que o
  proxy o corte.
- **Dimensione os discos para os originais, não para os derivados.** Os derivados são uma fração
  pequena de uma biblioteca de arquivos RAW e vídeo. O servidor guarda cada blob uma única vez, do
  jeito que o cliente o criptografou.
- **Usuários acrescentam banda, não processamento.** Pesquisa, miniaturas e organização rodam nos
  clientes. Mais pessoas navegando significa mais bytes servidos, mas quase nada a mais de CPU ou
  RAM.

`capsule-server/compose.yaml` roda o Postgres com as configurações certas para uso local
(`mise run serve-deps`). As credenciais dele são apenas para desenvolvimento.

<a id="build"></a>

## Compilação

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

Os binários fazem link com a glibc e não precisam de mais nada instalado. Execute-os como um usuário
sem privilégios cujo único caminho gravável seja o `BLOB_ROOT`, o mesmo formato que a imagem de
contêiner abaixo impõe.

<a id="container-image"></a>

## Imagem de contêiner

:::caution
Nenhuma imagem foi publicada ainda, e as rotas `/livez` e `/readyz` e o subcomando `healthcheck`
ainda não foram implementados. Esta seção é o contrato segundo o qual a imagem é construída.
:::

- **Base:** `gcr.io/distroless/cc-debian12:nonroot`, fixada por digest. Ela traz a glibc, o
  runtime do GCC, os certificados de CA e os dados de fuso horário, e nenhum shell ou gerenciador de
  pacotes.
- **Arquiteturas:** uma imagem multiarquitetura para `linux/amd64` e `linux/arm64`, cada uma
  compilada nativamente para o seu alvo glibc. As compilações não são otimizadas para uma CPU
  específica; o cálculo de hashes escolhe SHA-NI ou as extensões de criptografia do Armv8 em tempo
  de execução.
- **Conteúdo:** `capsule-server` (o entrypoint, `serve` por padrão) e `capsule-server-migration`,
  da mesma compilação.
- **Usuário:** não root, UID e GID 65532. O contêiner não precisa de nenhuma capability do Linux:
  remova todas e ative `no-new-privileges`.
- **Sistema de arquivos raiz somente leitura.** O servidor grava apenas em `BLOB_ROOT`, então monte
  o diretório de blobs como o único volume gravável, de propriedade do UID 65532, e monte a raiz
  como somente leitura.
- **Listener:** defina `SERVER_HOST=0.0.0.0` (ou `::`) dentro do contêiner; a porta 3000 é a
  padrão. Publique-a somente para o proxy reverso ([](#proxy-reverso)).
- **Logs** vão para o stderr; defina `LOG_FORMAT=json` para um coletor de logs.
- **Parada:** `serve` encerra de forma graciosa com `SIGTERM`. Um envio interrompido por uma parada
  retoma a partir dos intervalos já confirmados pelo servidor.
- **Uma réplica.** Rode exatamente um `serve` por diretório de blobs ([](#diretório-de-blobs)). No
  Kubernetes, use uma réplica com a estratégia de atualização `Recreate`: uma atualização gradual
  inicia o novo servidor enquanto o antigo ainda está atendendo.
- **As migrações** rodam a partir da mesma tag de imagem antes de o `serve` iniciar, como um Job,
  um init container ou um `ExecStartPre=`: `capsule-server-migration up`
  ([](#migre-e-depois-inicie)).
- **Os comandos de manutenção** rodam a partir da mesma imagem, com o mesmo usuário e no mesmo
  volume, como CronJobs ou timers ([](#manutenção)).
- **Saúde:** `/livez` responde a um `GET` enquanto o processo atende e não verifica mais nada, para
  uma liveness probe. `/readyz` só responde enquanto o PostgreSQL responde, o esquema corresponde ao
  binário e o `BLOB_ROOT` é gravável, para uma readiness probe. As duas ficam fora de `/v1`, então
  uma probe não depende da versão do protocolo, e nenhuma delas precisa ser acessível pelo proxy. A
  imagem não tem `curl`, então a verificação de saúde dela executa `capsule-server healthcheck`, que
  chama `/livez` no listener local.
- **Relógio:** o contêiner lê o relógio do host; sincronize o host ([](#relógio)).

<a id="configure"></a>

## Configuração

A configuração é feita somente por variáveis de ambiente. `capsule-server/.env.example` é um modelo
inicial, e esta página cita as variáveis que um operador precisa decidir. Um servidor durável se
recusa a iniciar (saída 2), citando todas as variáveis ausentes de uma vez, sem:

| Variável | Finalidade |
| --- | --- |
| `DATABASE_URL` | Conexão com o PostgreSQL |
| `BLOB_ROOT` | Diretório de blobs |
| `JWT_ED25519_DER` | Chave Ed25519 em Base64 que assina os tokens de sessão |
| `ATTESTATION_KEY_SEED` | Semente em Base64 que assina os recibos de custódia; precisa ser independente da chave de tokens |

- **Proteja a chave de tokens acima de tudo.** O banco de dados não guarda nenhum segredo de
  portador, então um banco de dados ou backup vazado não rende nenhuma sessão por si só;
  `JWT_ED25519_DER` é o que transforma esse vazamento em todas as sessões ativas
  ([](/design/authentication/#session-token)). A detecção de reutilização de tokens de atualização
  ([](/design/authentication/#token-lineage-and-reuse)) não ajuda contra isso, porque quem tem a
  chave emite tokens em vez de reaproveitá-los. Guarde-a no seu cofre de segredos, nunca no banco de
  dados nem nos backups dele, e faça a rotação depois de uma suspeita de vazamento, o que desconecta
  todos os dispositivos.
- **Uma chave de selagem do segundo fator está planejada.** O servidor vai selar os segredos TOTP
  em repouso com uma chave dedicada, independente da chave de tokens, para que um vazamento do banco
  de dados ou de um backup não renda nenhum segundo fator e a rotação da chave de tokens não deixe
  nenhum inutilizado ([](/design/authentication/#the-second-factor)). A configuração dela ainda não
  existe; quando chegar, mantenha-a separada do banco de dados e dos backups, como a chave de
  tokens.
- **Cadastro aberto exige uma cota padrão** (planejado). Quando as cotas forem configuráveis,
  `serve` vai se recusar a iniciar com o cadastro por senha ativado e sem um limite padrão, e um
  operador que não queira limite precisa defini-lo explicitamente como `unlimited`, o que é
  registrado em `WARN` a cada inicialização ([](/design/quota/#thresholds-and-states)). Hoje todas
  as implantações rodam sem limite.

`SYNC_CURSOR_MAC_KEY` é derivada de `JWT_ED25519_DER` quando não está definida, então a rotação da
chave de tokens também invalida os cursores de sincronização pendentes, a menos que você a defina
explicitamente. A federação fica desativada a não ser que `FEDERATION_ENABLED=true` e
`FEDERATION_URL` estejam definidas; deixe-a desativada.

Informe o seu proxy reverso para que os limites de taxa por origem vejam o cliente, e não o proxy.

| Variável | Significado |
| --- | --- |
| `TRUSTED_PROXIES` | Endereços e redes CIDR separados por vírgula (`10.0.0.0/8`). Uma rede `/0` é recusada, porque confiaria em qualquer remetente. |
| `TRUSTED_PROXY_HOPS` | Confia nessa quantidade de saltos de encaminhamento mais próximos do servidor, seja qual for o endereço deles. |
| `TRUSTED_PROXY_HEADER` | O único campo de encaminhamento que o servidor lê: `x-forwarded-for` (o padrão) ou `forwarded` (RFC 7239). Qualquer outro valor impede a inicialização. |

Sem essas variáveis, nenhum cabeçalho de encaminhamento é considerado. Isso é seguro, mas então
todos os clientes atrás do proxy dividem a cota do proxy, e um estranho tentando adivinhar senhas
deixa o login mais lento para todo mundo ([](/design/authentication/#sign-in-limits)); o servidor
registra um aviso quando vê um cabeçalho de encaminhamento sem confiar em nenhum salto. O erro
oposto é pior: confiar em um salto que não acrescenta o endereço do cliente permite que cada
cliente escolha a própria chave de limite de taxa. O campo que `TRUSTED_PROXY_HEADER` não cita é
ignorado, venha de quem vier, então um cliente não consegue escolher a própria chave de limite de
taxa enviando um campo que o seu proxy repasse. Informe o campo ao qual o seu proxy acrescenta o
endereço. Com o padrão, faça o seu proxy acrescentar a `X-Forwarded-For` o endereço que ele viu,
como o nginx faz com:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Defina `TRUSTED_PROXY_HEADER=forwarded` somente se o seu proxy acrescentar o próprio elemento a
`Forwarded` e não escrever `X-Forwarded-For`.

<a id="migrate-then-start"></a>

## Migre e depois inicie

`serve` nunca faz migração e recusa um esquema de banco de dados mais antigo que o binário. Execute
as migrações primeiro, a cada atualização:

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` grava os logs no stderr e exibe `listening on <url>` no stdout. Não use `--memory` em
produção: ele mantém tudo, exceto os blobs, na memória do processo.

<a id="maintenance"></a>

## Manutenção

Agende estes comandos de execução única (cron ou um timer). Eles precisam de `DATABASE_URL` e
`--blob-root`, e por padrão fazem uma simulação:

- `capsule-server gc --apply`: recupera blobs sem referência depois de um período de carência e
  exclui sessões, sessões de envio e contadores expirados. Execute regularmente, mesmo quando o
  armazenamento não estiver apertado.
- `capsule-server purge --apply`: elimina definitivamente os itens excluídos logicamente cujo prazo
  de retenção terminou. Ele também precisa do `ATTESTATION_KEY_SEED` do servidor, porque cada
  eliminação é antes assinada no registro do proprietário
  ([](/design/filesystem/server/#the-owner-ledger)), e se recusa a rodar sem ele. Termina com código
  diferente de zero quando o registro não conseguiu gravar uma eliminação; esse item é mantido para a
  próxima execução.
- `capsule-server scrub [--deep]`: verificação de integridade somente leitura do índice em relação
  ao armazenamento de blobs. Ele **termina com código diferente de zero a qualquer achado**, então
  crie um alerta para isso ([](/design/filesystem/maintenance/#server-side-integrity-scrub)).

Depois de restaurar um backup ou perder o banco de dados, reconstrua o índice a partir dos registros
dos proprietários em `BLOB_ROOT` ([](/design/filesystem/server/#rebuild)). Defina
`ATTESTATION_KEY_SEED` no host para que ele verifique as assinaturas dos registros.

1. `capsule-server rebuild` compara o índice com os registros.
2. Com o `serve` parado, `capsule-server rebuild --apply` restaura as linhas que faltam no índice.
   Ele nunca sobrescreve uma linha que o índice já tem. Ele reancora um registro que perdeu o final,
   e a partir daí o servidor volta a gravar para esse proprietário.
3. Se o relatório disser que um registro foi reescrito, o servidor recusa as escritas desse
   proprietário até você investigar e executar `capsule-server rebuild --apply --accept-rewrites`.

Um servidor que aplicou operações de ciclo de vida (exclusão, restauração, atualização de metadados,
derivado, substituição) antes de registrar o tamanho dos blobs delas mantém essas referências com
tamanho 0. Execute `capsule-server repair-sizes` uma vez para listá-las e depois
`capsule-server repair-sizes --apply` para preencher cada tamanho a partir do armazenamento de
blobs. É seguro executá-lo ao lado de um `serve` em execução, e cada item reparado é reenviado aos
clientes. Ele termina com código diferente de zero quando um blob está ausente ou vazio e deixa essa
referência como estava.

## Backups

O estado durável é o PostgreSQL mais o `BLOB_ROOT` (blobs e `ledger/`); faça backup dos dois,
juntos. O banco de dados também guarda as sessões de login e as sessões de envio em andamento, então
uma restauração mantém os usuários conectados. Guarde também as variáveis de chave acima no seu
cofre de segredos: elas não estão no banco de dados, e a identidade de assinatura e de atestação do
servidor depende delas.

Faça primeiro o backup do banco de dados, depois o dos blobs:

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

O `serve` pode continuar rodando. Um blob está no disco e a entrada dele acrescentada ao registro
antes que a linha do índice que o cita seja confirmada, então uma cópia dos blobs feita depois do
dump contém todos os blobs que o dump referencia, e possivelmente escritas que o dump não pegou. Não
execute `gc --apply` nem `purge --apply` enquanto o backup estiver rodando: qualquer um dos dois pode
remover um blob que o dump ainda referencia.

A reconstrução abaixo restaura os itens enviados entre o dump e a cópia dos blobs, porque ela
acrescenta todas as linhas que faltam no índice. Ela nunca altera uma linha que o dump já contém,
então uma exclusão, restauração ou edição de metadados feita nesse intervalo em um item mais antigo
não é aplicada. Uma reconstrução só informa essa alteração como uma linha divergente (e termina com
código diferente de zero) quando ela mudou os blobs do item; uma exclusão ou restauração passa
despercebida. Quando um backup precisar capturar todas as alterações, pare o `serve` enquanto faz as
duas metades. Se uma restauração informar linhas divergentes e o host antigo ainda estiver rodando,
faça o backup de novo dessa forma.

<a id="restoring-onto-a-new-host"></a>

### Restaurar em um novo host

1. Inicie um PostgreSQL vazio e defina os mesmos `JWT_ED25519_DER` e `ATTESTATION_KEY_SEED` do host
   antigo.
2. Restaure o banco de dados. O dump traz o esquema e o registro de migrações, então não execute
   `capsule-server-migration up` antes:
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Copie o diretório de blobs de volta para o `BLOB_ROOT`, em um único sistema de arquivos.
4. Com o `serve` parado, execute `capsule-server rebuild` e leia o relatório. Ele lista os itens
   enviados depois do dump; `capsule-server rebuild --apply` os restaura com os números de sequência
   que eles tinham, então os cursores de sincronização dos clientes continuam válidos.
5. Execute `capsule-server scrub --deep`. Ele não deve informar nada; termina com código diferente
   de zero a qualquer achado.
6. Inicie o `serve`. As sessões voltam com o banco de dados, então um cliente conectado no momento
   do dump continua conectado, a menos que a sessão dele tenha expirado desde então. Antes de
   começar a escutar, o `serve` descarta todas as sessões de envio inacabadas cujo arquivo
   temporário em `BLOB_ROOT` não foi restaurado, e exclui todos os arquivos temporários sem sessão;
   um cliente cujo envio foi descartado começa aquele arquivo de novo. Os clientes retomam a
   sincronização de onde pararam.

Se o banco de dados for perdido e não houver dump, migre um banco de dados vazio e execute
`capsule-server rebuild --apply` sobre o diretório de blobs. Ele restaura todas as linhas de itens a
partir dos registros dos proprietários. Contas, diretórios de dispositivos publicados, participação
em álbuns e links de compartilhamento ficam apenas no banco de dados, e uma reconstrução não os
restaura.

Ensaie esse processo antes de confiar dados reais a ele, por exemplo conferindo com
`capsule restore` que cada original volta byte a byte. `capsule-e2e/tests/backup_restore_drill.rs`
executa os mesmos passos contra contêineres (`mise run test-rust-containers`).
