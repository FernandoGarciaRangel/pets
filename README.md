<p align="center"><img src="assets/logo.png" width="160" alt="Logo do Pets: rosto de uma raposinha laranja"></p>

# Pets

Pets do Codex flutuando em qualquer tela do Windows, reagindo ao que o **Claude Code** e o **Codex** estão fazendo.

- Um exe nativo em Rust (~4,7 MB, dos quais ~3,8 MB são os pets embutidos; ~20 MB de RAM, CPU ≈ 0% parado): janela Win32 layered, sem webview nem GPU.
- Fica sempre no topo, em qualquer monitor; arraste para onde quiser e a posição é lembrada.
- Usa os pets que já estão em `~/.codex/pets/<pet>/` (`pet.json` + `spritesheet.webp`, o formato do Codex).
- Controle pelo ícone da raposa na bandeja ou com o clique direito no pet: trocar pet, esconder, iniciar com o Windows.
- Tamanho livre: menu **Tamanho…** abre um slider (48–512 px) que redimensiona ao vivo.
- Vem com uma biblioteca de pets originais do projeto, **Tatu-canastra** (o padrão) e **Draco**, instalados em
  `~/.codex/pets` na primeira execução e listados à parte no menu (**Do Pets** / **Seus pets**).

## Animações

| Situação | Animação |
| --- | --- |
| Algum agente trabalhando | `running` |
| Algum agente esperando você (permissão, pergunta) | `waiting` |
| Agente terminou (por 60 s) | `review` |
| Falha ao terminar (Claude, por 5 s) | `failed` |
| Nada acontecendo | `idle` |
| Mouse em cima | `jumping` |
| Arrastando | `running-left` / `running-right` |

Com várias sessões ao mesmo tempo, vale a mais urgente: `waiting` > `running` > `failed` > `review` > `idle`.

## Instalação

Baixe o zip da [última release](../../releases/latest), extraia e rode:

```powershell
.\pets.exe install
```

Isso copia o exe para `%LOCALAPPDATA%\Programs\Pets`, adiciona os hooks em `~/.claude/settings.json` e
`~/.codex/hooks.json` (com backup `.bak-pets`, sem mexer em hooks de outros apps), cria o atalho **Pets** no
Menu Iniciar e na área de trabalho e sobe o pet. O atalho da área de trabalho é criado uma vez só: se você
apagar, as atualizações não recriam.
O Codex pode pedir para confiar nos hooks novos na próxima sessão.

Para tirar os hooks: `pets.exe uninstall-hooks`.

## Atualizações

O app procura versão nova no GitHub ao iniciar e a cada 6 h. Quando encontra, mostra uma notificação e um item
**Atualizar para vX** no menu; ao clicar, baixa o `pets.exe` da release, confere o SHA-256 com o `SHA256SUMS.txt`
e se reinstala sozinho. Dá para desligar a checagem automática no menu ou forçar pela linha de comando:

```powershell
pets.exe check-update   # só consulta
pets.exe update         # consulta, baixa e instala
```

## Atualização dos pets

Pets instalados de **codex-pets.net**, **petdex.dev** ou **aimcp.info** (repositório
[awesome-codex-pet](https://github.com/legeling/awesome-codex-pet)) recebem as versões novas que os autores publicam.
Na primeira checagem cada pet é identificado pelo spritesheet idêntico ao do catálogo; pets criados localmente ou
diferentes do catálogo ficam de fora e nunca são substituídos. Quando sai versão nova, o menu mostra
**Atualizar pets (N)**; antes de trocar, o app valida a imagem nova e guarda os arquivos antigos em
`%APPDATA%\pets\backups`.

```powershell
pets.exe check-pets    # situação de cada pet
pets.exe update-pets   # aplica as versões novas
```

## Pets

Qualquer pasta em `~/.codex/pets` com um `pet.json` vira um pet. Sem layout no manifest, vale o grid do Codex:
quadros de 192×208, 8 colunas, linhas `idle`, `running-right`, `running-left`, `waving`, `jumping`, `failed`,
`waiting`, `running`, `review`. Manifests com `frame`/`animations` explícitos também funcionam.

Config em `%APPDATA%\pets\config.toml` (pet, largura, posição, visível, `pets_dir`).

## Desenvolvimento

```powershell
./dev.ps1            # debug no terminal atual, com log dos hooks recebidos
./dev.ps1 -Release   # build otimizado
./dev.ps1 -Install   # build release + pets.exe install
cargo test
```

Como funciona: `pets.exe hook <claude|codex>` é o comando que os agentes chamam. Ele lê o payload do hook no stdin,
manda o evento por um named pipe (`\\.\pipe\pets-agent-state`) para o app e sai na hora. Se o app não estiver
rodando, sai em silêncio e nunca bloqueia o agente.

## Release

O CI roda fmt, clippy e testes em todo push/PR. Para publicar, suba a versão no `Cargo.toml` e crie a tag:

```powershell
git tag v0.1.0
git push origin v0.1.0
```
