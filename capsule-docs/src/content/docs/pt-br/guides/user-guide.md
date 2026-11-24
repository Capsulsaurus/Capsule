---
title: Guia do usuário
description: O uso diário de uma biblioteca do Capsule com a CLI capsule — importar, fazer backup, verificar, organizar e excluir com segurança.
audit: none
---

Este guia cobre a CLI `capsule`, o cliente de referência. Os comandos marcados como *(planejado)*
estão no [roadmap](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md) e ainda não estão
disponíveis. `capsule <command> --help` é a referência oficial para as opções; a referência da CLI
([](/reference/cli/)) lista todas elas.

:::caution
O Capsule está em fase alfa. Até a primeira versão de uso interno, mantenha um backup independente
de qualquer original que você importar.
:::

<a id="set-up"></a>

## Configuração

1. Crie uma biblioteca: `capsule library init --name "Photos"`. A biblioteca é um diretório local
   criptografado; a frase secreta dela protege as chaves nesta máquina.
2. Entre no seu servidor: `capsule auth register` ou `capsule auth login` (`--oidc` ou `--device`
   para login único).
3. Configure a recuperação: `capsule auth escrow`. Guarde offline o segredo de recuperação exibido.
   Sem ele e sem a sua frase secreta, perder a máquina significa perder a biblioteca, porque o
   servidor não consegue descriptografar nada.

<a id="import-and-back-up"></a>

## Importar e fazer backup

- `capsule import <path>` copia arquivos para a biblioteca, assina a procedência e os metadados de
  cada item e gera miniaturas. `--provider takeout` importa um arquivo do Google Takeout com os
  metadados sidecar ([](/guides/google-photos-migration/)).
- `capsule push` envia a biblioteca. Os envios retomam de onde pararam, então executar `push` de
  novo é sempre seguro. Depois de cada item, `push` pede ao servidor que confirme que guarda de forma
  durável cada blob e mantém na biblioteca os recibos de custódia assinados pelo servidor. Um item
  que falha é relatado e a execução continua. O comando termina com código diferente de zero se algum
  item falhou, e `--json` lista o resultado de cada um.
- `capsule import --push` faz as duas coisas em uma etapa.
- **Mantenha seus originais até a custódia ser verificada.** Um item só está armazenado com
  segurança depois que o servidor informa que ele é durável e emite um recibo de custódia assinado,
  que `push` relata como verificado. Não exclua os arquivos de origem antes disso.
  `import --move --push` exclui cada origem somente depois que o item correspondente é verificado.
  Sem `--push`, `--move` mantém todas as origens e avisa, e um `import --move --push` posterior sobre
  os mesmos arquivos as libera.
- `capsule restore` *(planejado)* baixa os originais e os compara byte a byte com o que foi
  assinado na importação.

<a id="organize"></a>

## Organizar

- **Etiquetas** são livres e se ligam a um item, uma pilha (par RAW+JPEG, sequência de disparos,
  bracketing), um álbum ou uma região de uma imagem. As etiquetas que você adiciona ficam separadas
  das sugeridas pela IA, e você pode aceitar ou descartar uma sugestão. Etiquetas de itens já
  existem; etiquetas de pilhas, álbuns e regiões estão *(planejadas)*.
- `capsule show <asset>` mostra o que os metadados assinados de um item registram: legenda,
  classificação, etiquetas, localização e horário de captura.
- `capsule repair capture-time` encontra itens cujo horário de captura não bate com o EXIF do
  original e, com `--apply`, o corrige. A edição de localização está *(planejada)*.

Toda edição é uma alteração assinada e somente de acréscimo: o valor anterior permanece no histórico
de procedência do item, e edições simultâneas feitas em dois dispositivos são mescladas de forma
determinística.

<a id="delete-safely"></a>

## Excluir com segurança

Corrija metadados como a geolocalização antes de excluir, enquanto os itens ao redor ainda dão
contexto. Uma foto sem localização própria é fácil de situar enquanto as vizinhas do mesmo passeio
estão na biblioteca; quando elas somem, esse contexto vai junto.

- `capsule cull` marca itens como escolhido, neutro ou rejeitado e filtra pela marcação.
- `capsule cull --sweep` move os rejeitados para a lixeira. Itens na lixeira continuam
  recuperáveis durante o prazo de retenção (`--retain-days`) antes de serem eliminados.
- `capsule trash delete --asset <id>` move um item para a lixeira tanto no servidor quanto na
  biblioteca. Um item que já está na lixeira, por exemplo depois de `capsule cull --sweep`, mantém o
  prazo de retenção com que foi para a lixeira: `--retain-days` só se aplica a uma exclusão nova, e
  `--json` informa o prazo enviado como `retention_until`.
- `capsule trash restore --asset <id>` traz um item de volta. Uma restauração depois que o prazo
  de retenção terminou é recusada (`retention_elapsed` com `--json`) e não é tentada de novo; o item
  fica na lixeira até ser eliminado.

<a id="share"></a>

## Compartilhar

- `capsule share create --album <id>` ou `--asset <id>` gera um link somente para visualização;
  `--protect` adiciona uma frase secreta ao link e `--expires-at` uma data de expiração. O link leva
  a chave no fragmento da URL, que nunca chega ao servidor.
- `capsule share revoke <id>` impede que um link abra.

<a id="sync-another-machine"></a>

## Sincronizar outra máquina

- `capsule sync` baixa do servidor o feed de alterações da biblioteca; `capsule list` mostra o que
  ele trouxe.
- `capsule auth recover` traz a conta para uma máquina nova a partir do segredo de recuperação.
