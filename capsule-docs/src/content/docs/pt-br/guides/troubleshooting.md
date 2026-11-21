---
title: Solução de problemas
description: Colete a versão e os logs de que um relato precisa, e resolva os problemas comuns com um servidor do Capsule, uma importação ou uma biblioteca.
audit: none
---

Comece aqui quando algo der errado. Se nada abaixo resolver, [](/support/) diz onde perguntar, e os
diagnósticos da primeira seção são o que um relato de bug precisa.

<a id="collect-diagnostics"></a>

## Coletar diagnósticos

<a id="version"></a>

### Versão

Nenhuma versão foi lançada, então o número de versão não diferencia uma compilação de outra. Tudo é
compilado a partir do código-fonte, então informe o commit a partir do qual você compilou:
`git rev-parse --short HEAD` no seu checkout. A CLI não tem opção de versão, e
`capsule-server --version` mostra apenas a versão do crate, que é a mesma em todas as compilações.

### Logs

A CLI e o servidor gravam os logs em **stderr**. O stdout contém apenas o que um comando exibe, e com
`--json` isso é um único documento JSON, então você pode capturar cada um separadamente:

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` define o quanto é registrado. Uma compilação de release registra em `info` por padrão
  e uma de debug em `debug`. Use `RUST_LOG=debug` ao reproduzir um problema, e `RUST_LOG=trace`
  só quando solicitado: o volume é enorme.
- O formato de log do servidor é definido por `LOG_FORMAT`: `pretty` por padrão nas compilações de
  debug e `json` nas de release ([](/development/local-development/#logs)).
- A TUI em tela cheia (um `capsule` sem argumentos em um terminal) não consegue registrar logs na
  tela que ela desenha, então grava `tui.log` no diretório de cache da CLI, que `capsule status`
  exibe como Cache Directory. O arquivo é sobrescrito a cada sessão, então copie-o antes de iniciar
  a TUI de novo.
- A saída de `--json` é o registro mais preciso do que um comando fez, porque informa o resultado
  de cada item. Anexe-a quando um comando relatar falhas.

**Remova dados sensíveis antes de compartilhar.** Tire frases secretas, segredos de recuperação,
tokens, chaves, endereços de servidor e caminhos de arquivos pessoais. Nunca cole a frase secreta de
uma biblioteca ou um segredo de recuperação em lugar nenhum, nem mesmo em um relato privado.

<a id="the-server"></a>

## O servidor

<a id="it-refuses-to-start"></a>

### Ele não inicia

- **Status de saída 2, com nomes de variáveis.** Um servidor durável precisa de `DATABASE_URL`,
  `BLOB_ROOT`, `JWT_ED25519_DER` e `ATTESTATION_KEY_SEED`, e cita todas as que faltam de uma vez.
  Veja [](/guides/self-hosting/#configure).
- **O esquema do banco de dados é mais antigo que o binário.** `serve` nunca faz migração. Execute
  `capsule-server-migration up` a cada atualização, antes de `serve`
  ([](/guides/self-hosting/#migrate-then-start)).
- **Uma configuração de proxy confiável é recusada.** `TRUSTED_PROXIES` recusa uma rede `/0`, e
  `TRUSTED_PROXY_HEADER` aceita apenas `x-forwarded-for` ou `forwarded`.

Um servidor em execução exibe `listening on <url>` no stdout.

<a id="clients-cant-sign-in-or-connect"></a>

### Os clientes não conseguem entrar ou se conectar

- `capsule server-info` mostra como o servidor faz o login das pessoas: se ele tem um provedor de
  identidade e se recusa senhas. Um login com senha em um servidor com senhas desativadas falha por
  design.
- Atrás de um proxy reverso, todos os clientes compartilham o limite de taxa do proxy até que o
  proxy seja informado em `TRUSTED_PROXIES` ou `TRUSTED_PROXY_HOPS`. Quando várias pessoas atingem
  o limite ao mesmo tempo, verifique isso primeiro ([](/guides/self-hosting/#configure)).

<a id="the-index-and-the-blob-store-disagree"></a>

### O índice e o armazenamento de blobs não batem

`capsule-server scrub` é uma verificação de integridade somente leitura, e termina com código
diferente de zero a qualquer achado. Depois de restaurar um backup ou perder o banco de dados,
`capsule-server rebuild` compara o índice com os registros dos proprietários. Ele não altera nada até
você parar o `serve` e executar `capsule-server rebuild --apply`
([](/guides/self-hosting/#maintenance)).

<a id="imports-and-uploads"></a>

## Importações e envios

- **Um `push` parou no meio.** Execute de novo. Os envios retomam de onde pararam, então executar
  de novo é sempre seguro. `push` termina com código diferente de zero quando algum item falha, e
  `--json` lista o resultado de cada item ([](/guides/user-guide/#import-and-back-up)).
- **Mantenha seus originais até a custódia ser verificada.** Não exclua um arquivo de origem até
  que `push` informe que o item correspondente foi verificado, ou use `import --move --push`, que
  exclui cada origem somente depois que o item é verificado.
- **Uma exportação do Google Fotos parece incompleta.** Confira se todas as partes da exportação do
  Takeout foram baixadas e extraídas, e leia o que o importador não consegue garantir
  ([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **Os horários de captura estão errados.** `capsule repair capture-time` informa cada item cujo
  horário de captura assinado não bate com o EXIF do original, e `--apply` os corrige. Cada correção
  é um registro assinado permanente, então execute primeiro sem `--apply`.

<a id="check-a-librarys-integrity"></a>

## Verificar a integridade de uma biblioteca

Se parecer que há fotos ausentes ou danificadas, **pare e não exclua nada** enquanto investiga:
excluir pode remover o contexto de que você precisa para recuperá-las.

1. `capsule verify --library <path>` pergunta ao servidor se ele guarda de forma durável cada
   original. `--deep` faz o servidor recalcular o hash dos bytes armazenados em vez de apenas
   verificar se estão presentes, e `--signed` guarda na biblioteca o veredito assinado do servidor.
2. Itens na lixeira continuam recuperáveis durante o prazo de retenção:
   `capsule trash restore --library <path> --asset <id>` traz um de volta
   ([](/guides/user-guide/#delete-safely)).
3. A perda de um dispositivo ou de uma chave tem procedimentos próprios:
   [](/design/backup-recovery/#recovery-paths).

Se houver perda ou corrupção de dados, relate como bug com a saída de cada etapa. Perder ou
corromper dados é um defeito, seja qual for a causa.

<a id="still-stuck"></a>

## Ainda com problemas

[](/support/) lista onde fazer uma pergunta e como abrir um relato de bug sobre o qual seja possível
agir.
