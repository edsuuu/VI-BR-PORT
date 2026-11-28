# VI-BR-PORT: a versão para placas de 8 GB

[English](README.md) | **Português**

## Como usar

Dois downloads, cada um com a sua release na [página de Releases](https://github.com/edsuuu/VI-BR-PORT/releases):

| O quê | Release | Arquivo | Dentro |
|---|---|---|---|
| **Launcher** | `v<versão>` | `VI-BR-PORT-<versão>-windows-x64.zip` | `vi-launcher.exe` |
| **Engine** | `engine-v<versão>` | `VIctory-<versão>-windows-x64.zip` | `victory.exe` |

As releases que começam com `dev-` (`dev-v<versão>`, `dev-engine-v<versão>`) são builds de
teste, marcadas como pré-release; as sem prefixo são as estáveis.

1. Tenha **a sua própria cópia do jogo** (a versão de Xbox Series): uma pasta com o
   `game_scarlett_master.exe`.
2. Baixe as últimas releases do **launcher** e da **engine** e ponha o `vi-launcher.exe` e o
   `victory.exe` na mesma pasta (em outro lugar também funciona: a engine se escolhe nas
   configurações do launcher).
3. Abra o `vi-launcher.exe`.
4. Clique em **ESCOLHER PASTA DO JOGO** e escolha a pasta da sua cópia.
5. Se a engine não estiver ao lado do launcher, o botão vira **ESCOLHER A ENGINE**: escolha o
   `victory.exe`.
6. Clique em **JOGAR**. Na primeira vez os shaders são traduzidos e os pipelines aquecidos
   (alguns minutos, com a porcentagem na tela); depois o jogo abre e o launcher fica na bandeja.
7. **Mais opções**: a pasta dos saves, mouse e teclado (o Esc solta o cursor para os menus),
   flags extras, limpar os shaders compilados, procurar atualização.

Precisa de Windows 10 ou 11 (x64), processador com AVX2 e placa Direct3D 12 com shader model
6.6 e mesh shaders (testado numa NVIDIA de 8 GB), e uns 2 GB de disco para o cache de shaders.
Sem o launcher, o `victory.exe` roda com o `scriptsplay.ps1` (mais abaixo).

Camada de compatibilidade que roda no Windows, de forma nativa, a versão de
**Xbox Series X|S** (plataforma *Scarlett*, jogos feitos com o **GDK**) de um
título, a partir dos arquivos do próprio usuário. O código x86-64 do console
roda sem alteração dentro do nosso processo; por baixo dele ficam as bibliotecas
do sistema do console reescritas (HLE) e um back-end Direct3D 12 que recompila
os shaders da GPU do console (GFX10/RDNA2) para DXIL.

Este fork tem um alvo só: **placa NVIDIA de 8 GB com ondas de 32 lanes**
(testado numa RTX 4060 Ti) ligada a um **monitor 1080p**.

**Este repositório não contém arquivos, código ou recursos do jogo.** É preciso
ter a sua própria cópia. Nada aqui tem ligação com o desenvolvedor ou a
editora do jogo, nem é aprovado por eles.

## Onde está (2026-11-27)

**Jogável da abertura ao prólogo da Lucia na prisão** (a cela, o corredor, a sala comum, o
escritório da conselheira) **e dali até a primeira missão em mundo aberto (o barco)**, com
o jogador no controle, no perfil Series S.

**Vídeo** (NVIDIA RTX 4060 Ti 8 GB, a caminhada na prisão com o fps na tela; gravado em
26/11, antes dos ganhos abaixo): <https://youtu.be/HI7cEpJPhP4>

### Benchmark

Build release da `main`, aberto com o `scripts\play.ps1` (a configuração dele está em
[Como testar com o desempenho que estamos tendo](#como-testar-com-o-desempenho-que-estamos-tendo)),
perfil Series S, janela 1920x1080, upscaler do próprio jogo. PC: RTX 4060 Ti 8 GB (driver
621.40), i7-13700K, 64 GB de RAM, Windows 11. Uma máquina só. Cada número é a média das
linhas de quadro do log da rodada (uma a cada 5 segundos). A coluna de 26/11 é o commit
`8bb5142d` com a configuração daquele dia.

| Cena | Como foi medido | 26/11 | 27/11 |
|---|---|---|---|
| Prisão, andando | caminhada de 3 min por script (teclas pelo `VICTORY_KEYS_FILE`), a partir do save da cela | 24,0 fps (pior linha 16,3) | **26,6 a 27,9 fps** (pior linha 18,1 a 24,4; até 31% das linhas no teto de 30 do jogo) |
| Primeira missão em mundo aberto, o barco | 70 segundos pilotando o barco, a partir do save do escritório da conselheira | 20 a 22 fps | **28,7 a 29,1 fps** (pior linha 28,2) |
| Cutscene da abertura, parte 3D | os dois primeiros minutos de um jogo novo | 21,2 fps | **25,6 fps** (pior linha 18,5; a primeira cena roda a 30) |
| Prisão, personagem parada na cela | linhas de quadro do log, 35 s seguidos | 30,0 fps | **30,0 fps** (33,3 ms, o teto do jogo) |
| Vídeo da abertura | | 30 fps | 30 fps |

O alvo é 33 ms (30 fps) estáveis da abertura ao barco. Ainda não chegou. O barco fica um
quadro por segundo abaixo e segura o ritmo. A caminhada na prisão e a abertura batem no
teto nos trechos leves e caem a 18 a 24 fps nos pesados. No barco o limite é a placa (98%
ocupada). Na prisão e na abertura duas coisas estão no limite juntas: a placa (83 a 92%
ocupada) e a thread da camada que prepara as submissões (31 a 34 ms de trabalho por
quadro).

O que cada mudança deste fork mediu, antes e depois, com o commit de cada uma:
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).

O fps ao longo de uma rodada inteira, das linhas de quadro do próprio log (a faixa azul é a
caminhada por script; são rodadas de 26/11):

![fps na prisão, main](docs/img/fps-prisao-main.svg)

![fps na prisão com VICTORY_ASYNC_SUBMIT=1](docs/img/fps-prisao-async.svg)

Os gráficos são gerados dos logs; este repositório não guarda capturas do jogo
([docs/asset-policy.md](docs/asset-policy.md)).

**Estabilidade:** nenhuma perda da placa com esta configuração nas rodadas de 26 e 27/11
(mais de cem rodadas de 4 a 12 minutos), e uma queda do próprio jogo, aos quatro minutos
de uma rodada de 27/11, de causa ainda não encontrada. Sessões longas não foram medidas.

**Problemas conhecidos de imagem:**

- **Decorações assadas nos personagens saem erradas em algumas execuções**: quadrados
  coloridos na roupa (o short do homem no primeiro plano da abertura) ou na pele tatuada
  (os braços do guarda na prisão). O jogo assa essas texturas uma vez, a partir de
  texturas escolhidas por índice, e em algumas execuções uma das fontes ainda não está na
  placa. Cerca de uma execução em três com as listas finas, menos sem elas
  (`-More @{VICTORY_THIN='0'}`, com uns 2 fps a menos na prisão e no barco).
- **As texturas ficam moles no primeiro meio minuto de uma cena**: o jogo traz primeiro os
  níveis pequenos de cada textura e os grandes conforme o streaming anda, e o streaming
  dele roda mais devagar sob a camada do que no console (na abertura, mais de dez mil
  views de textura ainda esperam a memória aos 25 segundos).
- As cenas noturnas da abertura são iluminadas por um substituto da luz indireta por
  raios (uma constante com um piso), mais chapadas que no console.
- Num quadro ocasional as paredes somem ou a luz interna apaga.
- 35 programas de luz, sombra e neblina ainda são traduzidos de forma aproximada em placas
  de 32 lanes.

Corrigido desde 26/11: a sala comum da prisão (ficava escura debaixo da neblina; agora dá
para ver), texturas recusadas por falta de uma memória da placa que estava livre, o vidro
das janelas, o barco sem luz e os retângulos pretos no mar.

## O que este fork acrescenta

- **Listas finas na NVIDIA** (`VICTORY_THIN=1`): a chamada de lista só anota os
  argumentos e oito threads da camada traduzem ao lado do jogo. O passe que assa as
  decorações nas roupas é traduzido na própria chamada do jogo, na ordem do jogo
  (`VICTORY_THIN_BAKE`); fora de ordem ele deixava blocos de ruído nas roupas na maioria
  das execuções (em algumas ainda deixa: veja os problemas conhecidos).
- **Comparações na placa que leem menos** (`VICTORY_CARD_COMPARE_EVERY=30`): uma faixa de
  buffer do jogo que um shader pode ter escrito é lida inteira uma vez a cada 30
  submissões, e entre elas só os pedaços que mudaram há pouco (a abertura lia 2,4 GB de
  buffers por quadro para achar uns 30 pedaços mudados; agora lê 0,5 GB).
- **A reserva para render targets segue o que eles ocupam** (1,4 GB no Series S com o
  upscaler do jogo) em vez de 3 GB fixos de uma placa de 8 GB: texturas eram recusadas por
  falta de uma memória que estava livre.
- **As cópias que mudaram vêm de uma lista** (`VICTORY_CARD_TOUCHED=1`) em vez de uma
  olhada em cada uma das 3.700 cópias a cada submissão.
- **O substituto da luz indireta separa interior de exterior** (a palavra `room` do
  `VICTORY_AMBIENT_KEY` e o `VICTORY_AMBIENT_OUTSIDE`): uma constante fraca dentro dos
  cômodos, mais alta onde o cômodo está escuro; fora, a luz do próprio jogo com um piso.
- **`VICTORY_RT_MISS=1`**: o teste de raio não acerta nada; sem a estrutura de ray
  tracing montada, as sombras de luzes locais saíam de lixo (rosto preto ou com "purpurina").
- **Skinning do ray tracing pulado** com `RT_MISS=1`: os 30 programas `CSComputeSkinning*`
  (`bvh_skinning`) refaziam a pose de todo personagem só para a estrutura de ray tracing,
  que ninguém usa assim, e eram a maior parte do tempo da placa (~6,8 → ~10,9 fps no
  Series S). `VICTORY_RT_SKIN=1` volta a rodá-los.
- **Assado de decoração do rosto reconhecido pelo nome** (`PS_DecorationBlend_*`,
  `PS_BakeDecorations_*`, 51 programas): a lista de 4 hashes deixava de fora o que
  faz os rostos da cutscene, e o rosto saía preto.
- **Listas de memória alterada compactadas em vez de apagadas** quando enchem: apagar
  deixava texturas com bytes velhos depois de a memória mudar.
- **Composição de planos do present** (Series S: o mundo e a interface em planos separados),
  enviada à frente como o present de um plano só, sem esperar a placa.
- **Boca e expressões dos rostos**: a rede que anima os rostos guarda os valores em buffers
  de 16 bits e a tradução os lia como zero nas tabelas de descritores; agora são movidos
  como inteiros de 16 bits e a boca abre e fecha nas falas.
- **Sombras em cascata exatas em placas de 32 lanes** (NVIDIA) e **views de textura
  escritas pela placa** copiadas da própria textura.
- **Janela "Brazil | VIctory"** com a logo do fork (`VICTORY_ICON`) e
  **`VICTORY_KEYS_FILE`**: uma tecla escrita num arquivo é apertada no jogo sem precisar
  de foco (para scripts que passam pelos termos e pelo seletor de conta).
- **Vídeo da abertura sem voltar quadros**: cada lista tem o seu próprio buffer de upload
  para os planos de vídeo escritos pela CPU; com vários quadros em voo, um quadro
  sobrescrevia o anterior e os letreiros da abertura voltavam no meio do fade.
- **Textura nova só aparece depois do upload** (`VICTORY_SHOW_AFTER`, agora ligado; `0`
  desliga): um pedaço reaproveitado do pool mostrava bytes velhos, e o banco do carro
  da abertura virava um quadriculado verde e azul.
- **Patches do título também no release** (`VICTORY_UPSCALER=6`: o upscaler do próprio
  jogo no Series S, já que os passes do FidelityFX não têm DXIL aqui).
- **Pipelines**: pilha de 64 MB para o compilador do driver da NVIDIA, compute começando
  na criação, espera de 500 ms só nos 60 primeiros quadros (8 ms depois) e uma pipeline
  atrasada segurando só o primeiro quadro que a pede.
- **wave64 em placas de 32 lanes**: programas que pediriam ondas de 64 lanes usam a forma larga.
- **DirectStorage** grava em páginas somente leitura como o motor do console.
- `VICTORY_NO_WATCHDOG`, `VICTORY_NULL_MAGENTA` (diagnóstico) e o teto de 262.144
  faixas remapeadas.

## Como testar com o desempenho que estamos tendo

Numa janela normal do PowerShell (não como administrador: aí o driver guarda o cache de
shaders em outro lugar), na pasta do repositório:

```powershell
copy victory.example.toml victory.toml      # e aponte game_dir para a sua cópia
cargo build --locked --release -p victory
$env:VICTORY_RT_MISS = '1'                  # configuração de tradução: o cache de shaders depende dela
target\release\victory.exe shaders <os arquivos de shaders do jogo>
scripts\play.ps1 -Exe <o executável do jogo> -Build release
```

O passo `shaders` é preciso uma vez a cada build do tradutor (alguns minutos). Depois da
primeira rodada, `target\release\victory.exe warm --used` deixa prontas as pipelines com
que essa rodada desenhou (umas 4.000 aqui), para as rodadas seguintes não pararem para
criá-las; até lá os primeiros minutos num lugar novo são mais lentos. O título da janela
mostra fps, ms por quadro e draws por quadro; o log tem uma linha de quadro a cada 5
segundos (`... fps, ... ms/frame (... preparing ...)`).

O `scripts\play.ps1` liga o que foi usado no benchmark acima. As configurações que fazem
o fps, com o que cada uma mediu na RTX 4060 Ti (antes → depois):

| Configuração | O que faz | Medido aqui |
|---|---|---|
| `VICTORY_PORT_CONSTANTS=0` | o buffer de constantes de 182 MB do jogo é uma cópia na memória da placa, em vez de lido direto da memória do PC | barco 23,8 → 27,7 fps; caminhada na prisão 23,0 → 25,4 |
| `VICTORY_THIN=1` | a chamada de lista só anota os argumentos; oito threads da camada traduzem as chamadas ao lado do jogo | caminhada na prisão 25,3 → 27,5 fps (pior linha 24,2); barco 27,7 → 29,1 |
| `VICTORY_PORT_GUARD=1`, `VICTORY_PORT_JOURNAL=1` | a trava dos guardas é compartilhada, e o que os heaps guardam fica num diário | caminhada na prisão: 29,4 → 26,6 ms de preparo por quadro |
| `VICTORY_CARD_TOUCHED=1` | as cópias que mudaram vêm de uma lista, não de uma olhada nas 3.700 a cada submissão | uns 3 ms a menos de preparo por quadro |
| `VICTORY_CARD_COMPARE_EVERY=30` | uma faixa que um shader pode ter escrito é lida inteira na placa uma vez a cada 30 submissões, e entre elas só os pedaços que mudaram há pouco | abertura: 2,4 → 0,5 GB lidos por quadro, 9,9 → 1,8 ms de placa; com as listas finas, 21,7 → 25,6 fps |
| (sem configuração; `VICTORY_CARD_ROOM_FOR_TARGETS_MB` fixa à mão) | a reserva para render targets segue o que eles ocupam (1,4 GB) em vez de 3 GB fixos | "sem espaço" para textura 122 → 12 vezes por rodada; texturas deixadas com bytes errados 2.790 → 0 |
| `VICTORY_AMBIENT=constant:0.1:0.5`, `VICTORY_AMBIENT_OUTSIDE=1:0.16`, `VICTORY_AMBIENT_KEY=0.2:8:room:air:geo`, `VICTORY_AMBIENT_AIR=0.8` | o substituto da luz indireta por raios: uma constante fraca dentro dos cômodos, mais alta onde o cômodo está escuro; fora, a luz do próprio jogo com um piso | só imagem: a sala comum dá para ver e a rua à noite mantém o brilho dos postes |
| `VICTORY_RT_MISS=1`, `VICTORY_UPSCALER=6`, `VICTORY_MEMORY_CARD_MB=1024`, `VICTORY_DISPLAY_MODE=1920x1080`, `VICTORY_PIPELINE_BOOK=1`, `--console series-s` | todo teste de raio responde "não acertou nada"; o upscaler do próprio jogo; o perfil Series S em 1080p; as pipelines com que a rodada desenha ficam anotadas para o `warm` | a base de todas as medições |

Qualquer uma pode ser trocada numa rodada:
`scripts\play.ps1 -Exe ... -Build release -More @{VICTORY_THIN='0'}`. O `-Safe` roda sem
os caminhos mais novos (a primeira coisa a tentar quando algo sai errado). Em outra placa
tente também `-More @{VICTORY_PORT_CONSTANTS='1'}`: qual dos dois é mais rápido depende da
placa e do driver.

Testado e deixado desligado, porque mediu mais lento ou igual aqui:
`VICTORY_CARD_COMPARE_HOT` (a forma exata da comparação acima: 8 a 50 ms a mais de preparo
por quadro), `VICTORY_ASYNC_SUBMIT=1` (25,7 fps contra 25,6), `VICTORY_HEAP_RECHECK=0`,
`VICTORY_PORT_CONSTANTS_ORDER=1` (19,2 fps contra 23,0).

## Licença

GPL-3.0-or-later, como o VIctory. Ver [LICENSE](LICENSE).
