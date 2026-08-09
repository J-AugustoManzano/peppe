# PEPPE

**P**ortuguês **E**struturado **P**ara **P**rogramação **E**ducacional

Interpretador de uma linguagem de programação em português estruturado,
voltada para o ensino de algoritmos e lógica de programação, fundamentado sobre o livro:

```
Algorimos: Lógica para Desenvolvimento de Programação Imperativa de Computadores
Editora LTC
```

Fase de andamento do projeto:

> **Status:** em fase de beta-teste (semestre 2026).

## Download

Os binários prontos (Linux, Windows, macOS Intel e macOS Apple Silicon)
estão disponíveis na página de
[**Releases**](https://github.com/J-AugustoManzano/peppe/releases).

Basta baixar o arquivo correspondente ao seu sistema e rodar, não precisa
instalar nada além disso.

## Como usar

```
peppe caminho/para/programa.pe

ou

peppe programa.pe
```

O interpretador lê o arquivo `.pe`, verifica erros de sintaxe e semântica,
e executa o programa, mostrando erros com número de linha quando houver.

## Como compilar a partir do código-fonte

Requer o [Rust]([https://rust-lang.org/pt-BR/) instalado (`rustc`/`cargo`).

```
git clone https://github.com/J-AugustoManzano/peppe.git
cd peppe
cargo build --release
```

O executável fica em `target/release/peppe` (ou `peppe.exe` no Windows).

## Estrutura do projeto

- `peppe-core/` — biblioteca com o lexer, parser, verificador semântico e
  interpretador da linguagem
- `peppe-cli/` — interface de linha de comando que usa o `peppe-core`

## Compilação automática

Este repositório usa GitHub Actions para compilar automaticamente os
binários das quatro plataformas a cada nova versão. Veja
`.github/workflows/build.yml`.

## Autor

Augusto Manzano

## Licença

Este projeto está sob a licença [MIT](LICENSE). Uso, cópia e modificação
livres, desde que o aviso de direitos autorais e a licença original sejam
mantidos.
