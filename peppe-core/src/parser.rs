//! Parser (analisador sintático) da linguagem PEPPE — núcleo estrutural.
//!
//! Implementado como parser recursivo-descendente, um método por regra da
//! gramática. Programação Orientada a Objetos (`objeto`/`classe`)
//! ainda não é suportada: ao encontrar essas palavras-chave em posição de
//! declaração, o parser retorna um [`ErroSintatico`] explicando que o
//! recurso está previsto para uma fase futura, em vez de uma mensagem de
//! erro genérica.
//!
//! ## Decisões de implementação dignas de nota
//!
//! - **`;` é invisível** — o lexer já o trata como separador ignorável
//!, então o parser nunca precisa "esperar" por ele. Em
//!   particular, isso resolve sozinho a separação de grupos de parâmetros
//!   por `;` (Padrão A): um novo grupo de parâmetros começa
//!   sempre que o anterior termina e o próximo token não é `)`.
//! - **Padrão B de parâmetros** (`,` separando grupos — erro)
//!   é detectado especificamente em [`Parser::parse_parametros`], com uma
//!   mensagem didática explicando o motivo.
//! - **Atribuição vs. chamada de procedimento vs. rótulo** — todas começam
//!   com um identificador; [`Parser::parse_comando_identificador`] resolve
//!   a ambiguidade observando o que vem a seguir (`<-`, `(`, `:`, ou nada).
//! - **`escreva "texto: " leia VAR`** — funciona naturalmente: `escreva`
//!   para de consumir itens quando não há mais `,`, e o próximo `leia`
//!   inicia um novo comando no laço de [`Parser::parse_bloco`].

use crate::ast::*;
use crate::token::{Token, TokenKind};

/// Erro sintático, com posição (1-based) e mensagem didática em português.
#[derive(Debug, Clone, PartialEq)]
pub struct ErroSintatico {
    pub linha: usize,
    pub coluna: usize,
    pub mensagem: String,
}

impl std::fmt::Display for ErroSintatico {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Erro de sintaxe, linha {}, coluna {}: {}",
            self.linha, self.coluna, self.mensagem
        )
    }
}

/// Analisa a lista de tokens (terminada por [`TokenKind::FimDeArquivo`]) e
/// retorna a AST do programa, ou o primeiro erro sintático encontrado.
pub fn parsear(tokens: Vec<Token>) -> Result<Programa, ErroSintatico> {
    Parser::new(tokens).parse_programa()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        // Garantia mínima: sempre há ao menos o token FimDeArquivo.
        debug_assert!(!tokens.is_empty());
        Parser { tokens, pos: 0 }
    }

    // -- Utilitários de leitura ----------------------------------------------------

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Olha `offset` tokens à frente, sem nunca passar do último token
    /// (`FimDeArquivo`).
    fn peek_at(&self, offset: usize) -> &TokenKind {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    fn check(&self, kind: &TokenKind) -> bool {
        &self.peek().kind == kind
    }

    fn check_identificador(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Identificador(_))
    }

    fn linha_atual(&self) -> usize {
        self.peek().linha
    }

    fn coluna_atual(&self) -> usize {
        self.peek().coluna
    }

    fn avancar(&mut self) {
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn erro(&self, mensagem: impl Into<String>) -> ErroSintatico {
        ErroSintatico {
            linha: self.linha_atual(),
            coluna: self.coluna_atual(),
            mensagem: mensagem.into(),
        }
    }

    /// Consome o token atual se for `kind`, ou retorna erro didático.
    fn expect(&mut self, kind: TokenKind) -> Result<(), ErroSintatico> {
        if self.peek().kind == kind {
            self.avancar();
            Ok(())
        } else {
            Err(self.erro(format!(
                "esperava '{kind}', mas encontrei '{}'",
                self.peek().kind
            )))
        }
    }

    /// Consome um `Identificador` e retorna seu nome, ou erro didático.
    fn expect_identificador(&mut self) -> Result<String, ErroSintatico> {
        match self.peek().kind.clone() {
            TokenKind::Identificador(nome) => {
                self.avancar();
                Ok(nome)
            }
            outro => Err(self.erro(format!(
                "esperava um identificador (nome de variável, tipo ou sub-rotina), \
                 mas encontrei '{outro}'"
            ))),
        }
    }

    /// Retorna `Some(TipoPrimitivo)` se o token atual for uma das cinco
    /// palavras-chave de tipo primitivo — usado para detectar
    /// *casts*.
    fn tipo_primitivo_atual(&self) -> Option<TipoPrimitivo> {
        match self.peek().kind {
            TokenKind::TipoInteiro => Some(TipoPrimitivo::Inteiro),
            TokenKind::TipoReal => Some(TipoPrimitivo::Real),
            TokenKind::TipoCadeia => Some(TipoPrimitivo::Cadeia),
            TokenKind::TipoCaractere => Some(TipoPrimitivo::Caractere),
            TokenKind::TipoLogico => Some(TipoPrimitivo::Logico),
            _ => None,
        }
    }

    // =====================================================================================
    // Programa
    // =====================================================================================

    fn parse_programa(&mut self) -> Result<Programa, ErroSintatico> {
        self.expect(TokenKind::Programa)?;
        let nome = self.expect_identificador()?;
        let declaracoes = self.parse_declaracoes_topo()?;
        self.expect(TokenKind::Inicio)?;
        let bloco_principal = self.parse_bloco(&[TokenKind::Fim])?;
        self.expect(TokenKind::Fim)?;

        if !self.check(&TokenKind::FimDeArquivo) {
            return Err(self.erro(format!(
                "conteúdo inesperado após o 'fim' do programa: '{}'. \
                 Um programa PEPPE tem apenas um bloco 'início...fim' principal.",
                self.peek().kind
            )));
        }

        Ok(Programa { nome, declaracoes, bloco_principal })
    }

    // =====================================================================================
    // Declarações de nível superior (/ 9 — `const`, `tipo`, `var`, sub-rotinas)
    // =====================================================================================

    /// Consome `const`/`tipo`/`var`/`procedimento`/`função` em qualquer
    /// ordem e quantidade, até encontrar `início` (ou `fim` de
    /// uma sub-rotina sem corpo — não aplicável na v1, toda sub-rotina tem
    /// corpo). Retorna ao chamador quando nenhuma dessas palavras-chave
    /// inicia o próximo token.
    fn parse_declaracoes_topo(&mut self) -> Result<Vec<DeclaracaoTopo>, ErroSintatico> {
        let mut decls = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::Const => decls.extend(self.parse_secao_const()?),
                TokenKind::Tipo => decls.extend(self.parse_secao_tipo()?),
                TokenKind::Var => decls.extend(self.parse_secao_var()?),
                TokenKind::Objeto => decls.extend(self.parse_secao_objeto()?),
                TokenKind::Procedimento | TokenKind::Funcao => {
                    decls.push(self.parse_subrotina_ou_metodo_externo()?);
                }
                // 'NOME = <definição>' sem a palavra-chave 'tipo' repetida
                //: uma implementação de método externo
                // (`procedimento Classe..Método(...) ... fim`) pode
                // aparecer ENTRE duas declarações de classe que, no
                // código-fonte, fazem parte da mesma seção 'tipo' visual
                // — depois de processá-la, o próximo 'NOME = ...' ainda
                // pertence a essa mesma seção lógica, mesmo sem repetir
                // 'tipo'. Só entra aqui quando o identificador é
                // seguido de '=' (Padrão de `tipo`); qualquer
                // outra coisa (ex.: um comando começando antes de
                // 'início', o que já seria erro) cai no 'break' normal.
                TokenKind::Identificador(_) if self.peek_at(1) == &TokenKind::Igual => {
                    let linha = self.linha_atual();
                    let nome = self.expect_identificador()?;
                    self.expect(TokenKind::Igual)?;
                    let definicao = self.parse_tipo()?;
                    decls.push(DeclaracaoTopo::Tipo(DeclaracaoTipo { nome, definicao, linha }));
                }
                _ => break,
            }
        }
        Ok(decls)
    }

    /// `const NOME1 = <literal> NOME2 = <literal> ...`.
    fn parse_secao_const(&mut self) -> Result<Vec<DeclaracaoTopo>, ErroSintatico> {
        self.expect(TokenKind::Const)?;
        let mut decls = Vec::new();
        while self.check_identificador() {
            let linha = self.linha_atual();
            let nome = self.expect_identificador()?;
            self.expect(TokenKind::Igual)?;
            let valor = self.parse_literal()?;
            decls.push(DeclaracaoTopo::Const(DeclaracaoConst { nome, valor, linha }));
        }
        Ok(decls)
    }

    /// `tipo NOME1 = <definição> NOME2 = <definição> ...`.
    fn parse_secao_tipo(&mut self) -> Result<Vec<DeclaracaoTopo>, ErroSintatico> {
        self.expect(TokenKind::Tipo)?;
        let mut decls = Vec::new();
        while self.check_identificador() {
            let linha = self.linha_atual();
            let nome = self.expect_identificador()?;
            self.expect(TokenKind::Igual)?;
            let definicao = self.parse_tipo()?;
            decls.push(DeclaracaoTopo::Tipo(DeclaracaoTipo { nome, definicao, linha }));
        }
        Ok(decls)
    }

    /// `var <linha1> <linha2> ...`, onde cada linha é
    /// `NOME1, NOME2, ... : <tipo>`.
    fn parse_secao_var(&mut self) -> Result<Vec<DeclaracaoTopo>, ErroSintatico> {
        self.expect(TokenKind::Var)?;
        let mut decls = Vec::new();
        while self.check_identificador() {
            decls.push(DeclaracaoTopo::Var(self.parse_linha_var()?));
        }
        Ok(decls)
    }

    /// Uma linha `NOME1, NOME2, ... : <tipo>` — usada em `var` e campos de
    /// `registro`.
    fn parse_linha_var(&mut self) -> Result<DeclaracaoVar, ErroSintatico> {
        let linha = self.linha_atual();
        let mut nomes = vec![self.expect_identificador()?];
        while self.check(&TokenKind::Virgula) {
            self.avancar();
            nomes.push(self.expect_identificador()?);
        }
        self.expect(TokenKind::DoisPontos)?;
        let tipo = self.parse_tipo()?;
        Ok(DeclaracaoVar { nomes, tipo, linha })
    }

    /// `objeto NOME1, NOME2, ... : <Identificador_Classe>`.
    /// `objeto` e `var` são equivalentes para tipos-classe
    /// (ver especificação) — reaproveita exatamente a mesma
    /// gramática de `parse_secao_var`/`parse_linha_var`, só com a
    /// palavra-chave `objeto` introduzindo a seção em vez de `var`. O
    /// resultado é representado da mesma forma na AST
    /// (`DeclaracaoTopo::Var`) — não há distinção semântica a carregar.
    fn parse_secao_objeto(&mut self) -> Result<Vec<DeclaracaoTopo>, ErroSintatico> {
        self.expect(TokenKind::Objeto)?;
        let mut decls = Vec::new();
        while self.check_identificador() {
            decls.push(DeclaracaoTopo::Var(self.parse_linha_var()?));
        }
        Ok(decls)
    }

    // =====================================================================================
    // Tipos
    // =====================================================================================

    fn parse_tipo(&mut self) -> Result<Tipo, ErroSintatico> {
        if let Some(tp) = self.tipo_primitivo_atual() {
            self.avancar();
            return Ok(Tipo::Primitivo(tp));
        }
        match self.peek().kind.clone() {
            TokenKind::Generico => {
                self.avancar();
                Ok(Tipo::Generico)
            }
            TokenKind::Registro => self.parse_tipo_registro(),
            TokenKind::Conjunto => self.parse_tipo_conjunto(),
            TokenKind::Classe => self.parse_tipo_classe(),
            TokenKind::Funcao => self.parse_tipo_funcao(),
            TokenKind::Identificador(nome) => {
                self.avancar();
                Ok(Tipo::Nomeado(nome))
            }
            outro => Err(self.erro(format!(
                "esperava um tipo (inteiro, real, cadeia, caractere, lógico, generico, \
                 registro, conjunto ou o nome de um tipo definido com 'tipo'), \
                 mas encontrei '{outro}'"
            ))),
        }
    }

    /// `registro <campo1> <campo2> ... fim_registro`.
    fn parse_tipo_registro(&mut self) -> Result<Tipo, ErroSintatico> {
        self.expect(TokenKind::Registro)?;
        let mut campos = Vec::new();
        while self.check_identificador() {
            campos.push(self.parse_linha_var()?);
        }
        self.expect(TokenKind::FimRegistro)?;
        Ok(Tipo::Registro(campos))
    }

    /// `função(tipo1, tipo2, ...)` — tipo de uma
    /// referência a função de primeira classe. Só a lista de tipos dos
    /// parâmetros entra entre parênteses; o retorno é livre (não faz
    /// parte da sintaxe deste tipo). Lista vazia (`função()`) é válida
    /// — referencia uma função sem parâmetros.
    fn parse_tipo_funcao(&mut self) -> Result<Tipo, ErroSintatico> {
        self.expect(TokenKind::Funcao)?;
        self.expect(TokenKind::AbreParen)?;
        let mut parametros = Vec::new();
        if !self.check(&TokenKind::FechaParen) {
            parametros.push(self.parse_tipo()?);
            while self.check(&TokenKind::Virgula) {
                self.avancar();
                parametros.push(self.parse_tipo()?);
            }
        }
        self.expect(TokenKind::FechaParen)?;
        Ok(Tipo::Funcao { parametros })
    }

    /// `conjunto [<dim1>, <dim2>, ...] de <tipo>`, incluindo a
    /// forma dinâmica de uma ou mais dimensões:
    /// `conjunto [] de <tipo>` (1 dimensão dinâmica) ou, de forma geral,
    /// cada "slot" separado por vírgula dentro de `[...]` pode ser vazio
    /// (dimensão dinâmica, dimensionada depois via `dimensione`) ou um par
    /// `<início>..<fim>` (dimensão estática) — ex.: `conjunto [,] de
    /// <tipo>` para 2 dimensões dinâmicas, ou `conjunto [1..10,] de <tipo>`
    /// para uma matriz com a primeira dimensão fixa e a segunda dinâmica.
    fn parse_tipo_conjunto(&mut self) -> Result<Tipo, ErroSintatico> {
        self.expect(TokenKind::Conjunto)?;
        self.expect(TokenKind::AbreColchete)?;

        let mut dimensoes = Vec::new();
        loop {
            if self.check(&TokenKind::Virgula) || self.check(&TokenKind::FechaColchete) {
                // Slot vazio = dimensão dinâmica.
                dimensoes.push(None);
            } else {
                let inicio = self.parse_expr()?;
                self.expect(TokenKind::PontoPonto)?;
                let fim = self.parse_expr()?;
                dimensoes.push(Some((inicio, fim)));
            }

            if self.check(&TokenKind::Virgula) {
                self.avancar();
            } else {
                break;
            }
        }
        self.expect(TokenKind::FechaColchete)?;
        self.expect(TokenKind::De)?;
        let elemento = Box::new(self.parse_tipo()?);
        Ok(Tipo::Conjunto { dimensoes, elemento })
    }

    // =====================================================================================
    // Sub-rotinas
    // =====================================================================================

    /// `(procedimento|função) NOME[(<parâmetros>)] [: <tipo_retorno>]`
    /// `<declarações locais> início <corpo> fim`.
    /// Decide entre `procedimento`/`função` de nível superior comum
    /// e a forma de **método externo** `procedimento
    /// <Classe>..<MÉTODO>(...) ... fim` / `função <Classe>..<MÉTODO>(...)
    /// : <tipo> ... fim` — a diferença só aparece depois do
    /// primeiro identificador (nome da classe, no caso de método externo,
    /// ou nome da própria sub-rotina, no caso comum): se o token seguinte
    /// é `..` (resolução de escopo), é método externo.
    fn parse_subrotina_ou_metodo_externo(&mut self) -> Result<DeclaracaoTopo, ErroSintatico> {
        if matches!(self.peek_at(1), TokenKind::Identificador(_)) && self.peek_at(2) == &TokenKind::PontoPonto
        {
            let linha = self.linha_atual();
            let categoria = match self.peek().kind {
                TokenKind::Procedimento => CategoriaSubRotina::Procedimento,
                TokenKind::Funcao => CategoriaSubRotina::Funcao,
                _ => unreachable!("chamado apenas quando o token atual é 'procedimento'/'função'"),
            };
            self.avancar();
            let classe = self.expect_identificador()?;
            self.expect(TokenKind::PontoPonto)?;
            let nome = self.expect_identificador()?;

            let parametros = if self.check(&TokenKind::AbreParen) {
                self.parse_parametros()?
            } else {
                Vec::new()
            };
            let tipo_retorno = if categoria == CategoriaSubRotina::Funcao {
                self.expect(TokenKind::DoisPontos)?;
                Some(self.parse_tipo()?)
            } else {
                None
            };
            let declaracoes_locais = self.parse_declaracoes_topo()?;
            self.expect(TokenKind::Inicio)?;
            let corpo = self.parse_bloco(&[TokenKind::Fim])?;
            self.expect(TokenKind::Fim)?;

            return Ok(DeclaracaoTopo::MetodoExterno {
                classe,
                metodo: SubRotina {
                    categoria,
                    nome,
                    parametros,
                    tipo_retorno,
                    declaracoes_locais,
                    corpo,
                    linha,
                },
            });
        }

        Ok(DeclaracaoTopo::SubRotina(self.parse_subrotina()?))
    }

    fn parse_subrotina(&mut self) -> Result<SubRotina, ErroSintatico> {
        let linha = self.linha_atual();
        let categoria = match self.peek().kind {
            TokenKind::Procedimento => CategoriaSubRotina::Procedimento,
            TokenKind::Funcao => CategoriaSubRotina::Funcao,
            _ => unreachable!("chamado apenas quando o token atual é 'procedimento'/'função'"),
        };
        self.avancar();

        let nome = self.expect_identificador()?;

        let parametros = if self.check(&TokenKind::AbreParen) {
            self.parse_parametros()?
        } else {
            Vec::new()
        };

        let tipo_retorno = if categoria == CategoriaSubRotina::Funcao {
            self.expect(TokenKind::DoisPontos)?;
            Some(self.parse_tipo()?)
        } else {
            None
        };

        let declaracoes_locais = self.parse_declaracoes_topo()?;
        self.expect(TokenKind::Inicio)?;
        let corpo = self.parse_bloco(&[TokenKind::Fim])?;
        self.expect(TokenKind::Fim)?;

        Ok(SubRotina {
            categoria,
            nome,
            parametros,
            tipo_retorno,
            declaracoes_locais,
            corpo,
            linha,
        })
    }

    /// `classe [herança de <ClasseBase1>[, de <ClasseBase2>, ...]]
    /// <seções de membros>* fim_classe`. Herança múltipla
    /// Cada base adicional repete a palavra `de` depois da
    /// vírgula (`herança de CLS_A, de CLS_B`), não apenas uma lista de
    /// nomes separados por vírgula — assim a leitura fica inequívoca
    /// mesmo para quem só leu até aqui na ementa do curso.
    fn parse_tipo_classe(&mut self) -> Result<Tipo, ErroSintatico> {
        self.expect(TokenKind::Classe)?;

        let mut heranca = Vec::new();
        if self.check(&TokenKind::Heranca) {
            self.avancar();
            self.expect(TokenKind::De)?;
            heranca.push(self.expect_identificador()?);
            while self.check(&TokenKind::Virgula) {
                self.avancar();
                self.expect(TokenKind::De)?;
                heranca.push(self.expect_identificador()?);
            }
        }

        let mut membros = Vec::new();
        loop {
            let visibilidade = match self.peek().kind {
                TokenKind::SecaoPublica => Visibilidade::Publica,
                TokenKind::SecaoProtegida => Visibilidade::Protegida,
                TokenKind::SecaoPrivada => Visibilidade::Privada,
                _ => break,
            };
            self.avancar();
            while self.check_identificador()
                || self.check(&TokenKind::Procedimento)
                || self.check(&TokenKind::Funcao)
                || self.check(&TokenKind::Virtual)
                || self.check(&TokenKind::Sobrepor)
            {
                membros.push(self.parse_membro_classe(visibilidade)?);
            }
        }

        self.expect(TokenKind::FimClasse)?;
        Ok(Tipo::Classe { heranca, membros })
    }

    /// Um único membro dentro de uma seção de visibilidade:
    /// campo (`NOME1, NOME2 : <tipo>`) ou método — assinatura apenas
    /// (`procedimento NOME(...)`/`função NOME(...) : <tipo>`, sem corpo,
    /// implementado em outro lugar ) ou método interno
    /// (mesma assinatura seguida de `[declarações locais] início ... fim`,
    /// corpo implementado ali mesmo).
    fn parse_membro_classe(&mut self, visibilidade: Visibilidade) -> Result<MembroClasse, ErroSintatico> {
        if self.check_identificador() {
            let campo = self.parse_linha_var()?;
            return Ok(MembroClasse { visibilidade, item: ItemClasse::Campo(campo) });
        }

        // 'virtual'/'sobrepor' — modificador de dispatch,
        // sempre antes de 'procedimento'/'função'. Ausência de ambos =
        // 'Modificador::Nenhum' (binding estático, comportamento padrão).
        let modificador = match self.peek().kind {
            TokenKind::Virtual => {
                self.avancar();
                Modificador::Virtual
            }
            TokenKind::Sobrepor => {
                self.avancar();
                Modificador::Sobrepor
            }
            _ => Modificador::Nenhum,
        };

        let linha = self.linha_atual();
        let categoria = match self.peek().kind {
            TokenKind::Procedimento => CategoriaSubRotina::Procedimento,
            TokenKind::Funcao => CategoriaSubRotina::Funcao,
            _ => unreachable!("chamado apenas quando o token atual é identificador/'procedimento'/'função'/'virtual'/'sobrepor'"),
        };
        self.avancar();
        let nome = self.expect_identificador()?;
        let parametros = if self.check(&TokenKind::AbreParen) {
            self.parse_parametros()?
        } else {
            Vec::new()
        };
        let tipo_retorno = if categoria == CategoriaSubRotina::Funcao {
            self.expect(TokenKind::DoisPontos)?;
            Some(self.parse_tipo()?)
        } else {
            None
        };

        // Distingue assinatura-apenas de método interno: se o que vem a
        // seguir poderia iniciar declarações locais (exceto
        // 'procedimento'/'função' — ver nota abaixo) ou já é 'início'
        // diretamente, há um corpo aqui — é método interno.
        // Caso contrário (próximo token é outra assinatura/método,
        // outra seção de visibilidade, ou 'fim_classe'), é só a
        // assinatura, implementada em outro lugar.
        //
        // NOTA: diferente de 'parse_subrotina' (sub-rotina solta, onde
        // 'procedimento'/'função' podem iniciar uma sub-rotina ANINHADA
        // local dentro do corpo), aqui dentro de uma classe esses dois
        // tokens SEMPRE significam "começou o próximo membro" — um
        // método de classe não declara sub-rotinas locais aninhadas
        // (não há esse recurso no material de origem), então incluí-los
        // em 'tem_corpo' causaria ambiguidade real: duas assinaturas
        // consecutivas (`procedimento A(...)` seguido de `função
        // B(...)`) seriam erradamente lidas como "A tem corpo, e B é
        // uma declaração local dentro do corpo de A".
        let tem_corpo = matches!(
            self.peek().kind,
            TokenKind::Inicio | TokenKind::Const | TokenKind::Tipo | TokenKind::Var
        );

        if !tem_corpo {
            return Ok(MembroClasse {
                visibilidade,
                item: ItemClasse::AssinaturaMetodo {
                    categoria,
                    nome,
                    parametros,
                    tipo_retorno,
                    modificador,
                    linha,
                },
            });
        }

        let declaracoes_locais = self.parse_declaracoes_topo()?;
        self.expect(TokenKind::Inicio)?;
        let corpo = self.parse_bloco(&[TokenKind::Fim])?;
        self.expect(TokenKind::Fim)?;

        Ok(MembroClasse {
            visibilidade,
            item: ItemClasse::MetodoInterno(
                SubRotina {
                    categoria,
                    nome,
                    parametros,
                    tipo_retorno,
                    declaracoes_locais,
                    corpo,
                    linha,
                },
                modificador,
            ),
        })
    }


    ///
    /// `ref` marca passagem por referência; `vlr` marca passagem
    /// por valor de forma **explícita, porém opcional** — omitir o
    /// marcador já significa passagem por valor (o comportamento de
    /// `vlr X : tipo` e `X : tipo` é idêntico). Nem `ref` nem `var` (este
    /// último reservado à seção de declarações) se sobrepõem.
    ///
    /// Como `;` é invisível para o parser (descartado pelo lexer), um novo
    /// grupo simplesmente começa quando o anterior termina e o próximo
    /// token não é `)`. Se o próximo token for `,`, é o **Padrão B**
    /// — erro com mensagem explicativa.
    fn parse_parametros(&mut self) -> Result<Vec<Parametro>, ErroSintatico> {
        self.expect(TokenKind::AbreParen)?;
        let mut params = Vec::new();

        if !self.check(&TokenKind::FechaParen) {
            loop {
                let por_referencia = if self.check(&TokenKind::Ref) {
                    self.avancar();
                    true
                } else if self.check(&TokenKind::Var) {
                    // 'var' como marcador de passagem por referência —
                    // sinônimo de 'ref', estilo Pascal.
                    self.avancar();
                    true
                } else if self.check(&TokenKind::Vlr) {
                    self.avancar(); // puramente redundante — mesmo efeito de omitir
                    false
                } else {
                    false
                };

                let mut nomes = vec![self.expect_identificador()?];
                while self.check(&TokenKind::Virgula) {
                    self.avancar();
                    nomes.push(self.expect_identificador()?);
                }
                self.expect(TokenKind::DoisPontos)?;
                let tipo = self.parse_tipo()?;
                params.push(Parametro { nomes, tipo, por_referencia });

                if self.check(&TokenKind::FechaParen) {
                    break;
                }
                if self.check(&TokenKind::Virgula) {
                    return Err(self.erro(
                        "separador de grupos de parâmetros deve ser ';' (ponto e vírgula), \
                         não ','. Use ',' apenas para agrupar nomes do mesmo tipo dentro de \
                         um grupo — ex.: 'A, B : real; C : caractere'.",
                    ));
                }
                // Qualquer outro token ('ref', 'vlr' ou um identificador)
                // inicia um novo grupo de parâmetros — o ';' que o separava
                // já foi descartado pelo lexer.
            }
        }

        self.expect(TokenKind::FechaParen)?;
        Ok(params)
    }

    // =====================================================================================
    // Blocos e comandos 
    // =====================================================================================

    /// Lê comandos até que o token atual seja um dos `terminadores`
    /// (não-consumido — o chamador faz `expect`).
    ///
    /// Dois casos de erro são tratados especialmente, para evitar a mensagem
    /// genérica "comando inesperado":
    /// - Fim de arquivo antes do esperado.
    /// - `fim` (do programa ou de uma sub-rotina) aparecendo onde um
    ///   terminador específico do bloco era esperado — sinal quase certo de
    ///   um `fim_se`/`fim_para`/`fim_enquanto`/... esquecido.
    fn parse_bloco(&mut self, terminadores: &[TokenKind]) -> Result<Bloco, ErroSintatico> {
        let mut bloco = Vec::new();
        loop {
            if terminadores.iter().any(|t| self.check(t)) {
                break;
            }
            if self.check(&TokenKind::FimDeArquivo) || self.check(&TokenKind::Fim) {
                let opcoes: Vec<String> =
                    terminadores.iter().map(|t| format!("'{t}'")).collect();
                return Err(self.erro(format!(
                    "encontrei '{}' antes do esperado — faltou {}",
                    self.peek().kind,
                    opcoes.join(" ou ")
                )));
            }
            bloco.push(self.parse_comando()?);
        }
        Ok(bloco)
    }

    fn parse_comando(&mut self) -> Result<Comando, ErroSintatico> {
        match self.peek().kind {
            TokenKind::Identificador(_) => self.parse_comando_identificador(),
            TokenKind::Este => self.parse_comando_este(),
            TokenKind::Leia => self.parse_leia(),
            TokenKind::LeiaSeco => self.parse_leia_seco(),
            TokenKind::Escreva | TokenKind::EscrevaLn => self.parse_escreva(),
            TokenKind::Pausa => {
                let linha = self.linha_atual();
                self.avancar();
                Ok(Comando::Pausa { linha })
            }
            TokenKind::Se => self.parse_se(),
            TokenKind::ExcetoSe => self.parse_exceto_se(),
            TokenKind::Caso => self.parse_caso(),
            TokenKind::Enquanto => self.parse_enquanto(),
            TokenKind::AteSeja => self.parse_ate_seja(),
            TokenKind::Repita => self.parse_repita(),
            TokenKind::Execute => self.parse_execute(),
            TokenKind::Laco => self.parse_laco(),
            TokenKind::Para => self.parse_para(),
            TokenKind::Dimensione => self.parse_dimensione(),
            TokenKind::IrPara => self.parse_ir_para(),
            TokenKind::Interrompa => {
                let linha = self.linha_atual();
                self.avancar();
                Ok(Comando::Interrompa { linha })
            }
            TokenKind::Continue => {
                let linha = self.linha_atual();
                self.avancar();
                Ok(Comando::Continue { linha })
            }
            TokenKind::SaiaCaso => self.parse_saia_caso(),
            TokenKind::Limpar => {
                let linha = self.linha_atual();
                self.avancar();
                Ok(Comando::Limpar { linha })
            }
            TokenKind::LimparLinha => self.parse_limpar_linha(),
            TokenKind::Posicionar => self.parse_posicionar(),
            TokenKind::CorFundo => self.parse_cor_fundo(),
            TokenKind::CorFrente => self.parse_cor_frente(),
            ref outro => Err(self.erro(format!(
                "comando inesperado: '{outro}'. Esperava o início de um comando \
                 (atribuição, 'leia', 'escreva', 'se', um laço, etc.)."
            ))),
        }
    }

    /// Resolve a ambiguidade de um comando que começa com `Identificador`:
    /// atribuição (`X <- ...`, `A[I] <- ...`, `ALUNO.NOME <- ...`), chamada de
    /// procedimento (`NOME(args)`, sempre com parênteses mesmo sem
    /// argumentos) ou rótulo (`NOME:`)  (rótulos) e 9.7 (chamadas).
    fn parse_comando_identificador(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        let primeiro = self.expect_identificador()?;
        let (qualificador_base, nome) = self.parse_qualificador_base_opcional(primeiro)?;
        let acessos = self.parse_acessos()?;

        if self.check(&TokenKind::Seta) {
            self.avancar();
            let valor = self.parse_expr()?;
            return Ok(Comando::Atribuicao {
                destino: LValue { qualificador_base, nome, acessos, linha },
                valor,
                linha,
            });
        }

        if qualificador_base.is_none() && acessos.is_empty() && self.check(&TokenKind::DoisPontos) {
            self.avancar();
            return Ok(Comando::Rotulo { nome, linha });
        }

        if matches!(acessos.last(), Some(Acesso::Metodo { .. })) {
            // 'OBJETO.MÉTODO(args)' como comando solto — o
            // método já foi totalmente consumido (nome + argumentos)
            // dentro de 'parse_acessos'; aqui só embrulhamos a cadeia
            // completa de acessos no comando dedicado.
            return Ok(Comando::ChamadaMetodo {
                alvo: LValue { qualificador_base, nome, acessos, linha },
                linha,
            });
        }

        if qualificador_base.is_none() && self.check(&TokenKind::AbreParen) {
            let argumentos = self.parse_lista_argumentos()?;
            return Ok(Comando::ChamadaProcedimento { nome, argumentos, linha });
        }

        // Nenhuma forma válida de comando reconhecida a partir deste
        // identificador. Aponta para a linha onde o identificador foi lido
        // (não para onde o parser desistiu), que é o ponto mais útil para
        // o estudante localizar o erro no código-fonte.
        Err(ErroSintatico {
            linha,
            coluna: 1,
            mensagem: format!(
                "desconheço a cláusula: \"{}{}\".",
                nome,
                descrever_acessos(&acessos)
            ),
        })
    }

    /// `este.CAMPO <- valor` ou `este.MÉTODO(args)` como comando (seção
    /// 10.3/10.4) — versão de [`Self::parse_comando_identificador`]
    /// restrita ao caso de `este`, que nunca é chamável como
    /// `este(...)` nem usável como rótulo (`este:`), diferente de um
    /// identificador comum.
    fn parse_comando_este(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Este)?;
        let nome = "este".to_string();
        let acessos = self.parse_acessos()?;

        if self.check(&TokenKind::Seta) {
            self.avancar();
            let valor = self.parse_expr()?;
            return Ok(Comando::Atribuicao {
                destino: LValue { qualificador_base: None, nome, acessos, linha },
                valor,
                linha,
            });
        }

        if matches!(acessos.last(), Some(Acesso::Metodo { .. })) {
            return Ok(Comando::ChamadaMetodo {
                alvo: LValue { qualificador_base: None, nome, acessos, linha },
                linha,
            });
        }

        Err(self.erro(format!(
            "esperava '<-' (para formar uma atribuição) após 'este{}', \
             mas encontrei '{}'.",
            descrever_acessos(&acessos),
            self.peek().kind
        )))
    }

    /// `( [<expr> {, <expr>}] )` — lista de argumentos de uma chamada.
    fn parse_lista_argumentos(&mut self) -> Result<Vec<Expr>, ErroSintatico> {
        self.expect(TokenKind::AbreParen)?;
        let mut argumentos = Vec::new();
        if !self.check(&TokenKind::FechaParen) {
            argumentos.push(self.parse_expr()?);
            while self.check(&TokenKind::Virgula) {
                self.avancar();
                argumentos.push(self.parse_expr()?);
            }
        }
        self.expect(TokenKind::FechaParen)?;
        Ok(argumentos)
    }

    // -- leia / leia_seco / escreva ---------------------------------------

    /// `leia <lvalue> {, <lvalue>}`.
    fn parse_leia(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Leia)?;
        let mut variaveis = vec![self.parse_lvalue()?];
        while self.check(&TokenKind::Virgula) {
            self.avancar();
            variaveis.push(self.parse_lvalue()?);
        }
        Ok(Comando::Leia { variaveis, linha })
    }

    /// `leia_seco <lvalue>`.
    fn parse_leia_seco(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::LeiaSeco)?;
        let variavel = self.parse_lvalue()?;
        Ok(Comando::LeiaSeco { variavel, linha })
    }

    /// `escreva <item> {, <item>}` ou `escreva_ln [<item> {, <item>}]`,
    /// onde `<item>` é `<expr> [: <largura> [: <decimais>]]` (seção
    /// 6.2/6.2.1/6.2.2). `escreva` exige ao menos um item; `escreva_ln`
    /// aceita lista vazia (imprime apenas a quebra de linha).
    fn parse_escreva(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        let quebra_linha = self.check(&TokenKind::EscrevaLn);
        self.avancar(); // consome 'escreva' ou 'escreva_ln'

        // 'escreva_ln' sozinho (sem nenhum item) é válido — só a quebra de
        // linha. Detectamos isso de duas formas:
        // 1. O próximo token não pode iniciar uma expressão; OU
        // 2. O próximo token está em uma linha diferente (separação visual
        //    — ex.: 'escreva_ln' sozinho na linha 18, e 'SB <- ...' na
        //    linha 20: o 'SB' não é argumento do 'escreva_ln').
        // 'escreva' sozinho continua exigindo ao menos um item.
        if quebra_linha
            && (!self.proximo_token_inicia_expressao()
                || self.peek().linha > linha)
        {
            return Ok(Comando::Escreva { itens: Vec::new(), quebra_linha, linha });
        }

        let mut itens = vec![self.parse_item_escreva()?];
        while self.check(&TokenKind::Virgula) {
            self.avancar();
            itens.push(self.parse_item_escreva()?);
        }
        Ok(Comando::Escreva { itens, quebra_linha, linha })
    }

    /// `true` se o token atual é um dos que iniciam `parse_expr_primaria`
    /// (literais, identificador, `(`, ou uma palavra-chave de tipo
    /// primitivo iniciando um *cast*) — usado apenas para decidir se
    /// `escreva_ln` veio sem nenhum item.
    fn proximo_token_inicia_expressao(&self) -> bool {
        if self.tipo_primitivo_atual().is_some() {
            return true;
        }
        matches!(
            self.peek().kind,
            TokenKind::Inteiro(_)
                | TokenKind::Real(_)
                | TokenKind::Texto(_)
                | TokenKind::Caractere(_)
                | TokenKind::Logico(_)
                | TokenKind::AbreParen
                | TokenKind::Identificador(_)
                | TokenKind::Menos
                | TokenKind::Nao
        )
    }

    fn parse_item_escreva(&mut self) -> Result<ItemEscreva, ErroSintatico> {
        let expressao = self.parse_expr()?;
        let mut largura = None;
        let mut decimais = None;
        if self.check(&TokenKind::DoisPontos) {
            self.avancar();
            largura = Some(self.parse_expr()?);
            if self.check(&TokenKind::DoisPontos) {
                self.avancar();
                decimais = Some(self.parse_expr()?);
            }
        }
        Ok(ItemEscreva { expressao, largura, decimais })
    }

    // -- Condicionais -----------------------------------------------------

    /// `se (<cond>) então <bloco> [senão <bloco>] fim_se`.
    /// `se <cond> então <bloco> [senão <bloco>] fim_se`.
    /// A condição é qualquer expressão lógica — parênteses são
    /// opcionais e ficam a critério de quem escreve: tanto `se (A) .e.
    /// (B) então` quanto `se ((A) .e. (B)) então` e `se A .e. B então`
    /// são formas válidas e equivalentes, já que `Self::parse_expr` já
    /// resolve a precedência entre relacionais e `.e.`/`.ou.`/`.xou.`
    /// corretamente sem precisar de um parêntese externo delimitando
    /// onde a condição termina.
    fn parse_se(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Se)?;
        let condicao = self.parse_expr()?;
        self.expect(TokenKind::Entao)?;
        let entao = self.parse_bloco(&[TokenKind::Senao, TokenKind::FimSe])?;
        let senao = if self.check(&TokenKind::Senao) {
            self.avancar();
            Some(self.parse_bloco(&[TokenKind::FimSe])?)
        } else {
            None
        };
        self.expect(TokenKind::FimSe)?;
        Ok(Comando::Se { condicao, entao, senao, linha })
    }

    /// `exceto_se <cond> então <bloco> [senão <bloco>] fim_exceto_se`
    ///. Parênteses na condição são opcionais (mesma nota de
    /// `Self::parse_se`).
    fn parse_exceto_se(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::ExcetoSe)?;
        let condicao = self.parse_expr()?;
        self.expect(TokenKind::Entao)?;
        let entao = self.parse_bloco(&[TokenKind::Senao, TokenKind::FimExcetoSe])?;
        let senao = if self.check(&TokenKind::Senao) {
            self.avancar();
            Some(self.parse_bloco(&[TokenKind::FimExcetoSe])?)
        } else {
            None
        };
        self.expect(TokenKind::FimExcetoSe)?;
        Ok(Comando::ExcetoSe { condicao, entao, senao, linha })
    }

    /// `caso <expr> {seja <literal> faça <bloco_ou_comando>} [senão <bloco>]
    /// fim_caso`. `senão` é opcional.
    fn parse_caso(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Caso)?;
        let expressao = self.parse_expr()?;

        let mut ramos = Vec::new();
        while self.check(&TokenKind::Seja) {
            let linha_ramo = self.linha_atual();
            self.avancar();
            let valor = self.parse_literal()?;
            self.expect(TokenKind::Faca)?;
            // "bloco_ou_comando": o mesmo parse_bloco cobre os dois casos —
            // termina ao encontrar 'seja', 'senão' ou 'fim_caso', mesmo que
            // isso ocorra após apenas um comando.
            let corpo =
                self.parse_bloco(&[TokenKind::Seja, TokenKind::Senao, TokenKind::FimCaso])?;
            ramos.push(RamoCaso { valor, corpo, linha: linha_ramo });
        }

        let senao = if self.check(&TokenKind::Senao) {
            self.avancar();
            Some(self.parse_bloco(&[TokenKind::FimCaso])?)
        } else {
            None
        };

        self.expect(TokenKind::FimCaso)?;
        Ok(Comando::Caso { expressao, ramos, senao, linha })
    }

    // -- Laços --------------------------------------------------------------

    /// `enquanto <cond> faça <bloco> fim_enquanto`. Parênteses na
    /// condição são opcionais (mesma nota de `Self::parse_se`).
    fn parse_enquanto(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Enquanto)?;
        let condicao = self.parse_expr()?;
        self.expect(TokenKind::Faca)?;
        let corpo = self.parse_bloco(&[TokenKind::FimEnquanto])?;
        self.expect(TokenKind::FimEnquanto)?;
        Ok(Comando::Enquanto { condicao, corpo, linha })
    }

    /// `até_seja <cond> efetue <bloco> fim_até_seja`. Parênteses na
    /// condição são opcionais (mesma nota de `Self::parse_se`).
    fn parse_ate_seja(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::AteSeja)?;
        let condicao = self.parse_expr()?;
        self.expect(TokenKind::Efetue)?;
        let corpo = self.parse_bloco(&[TokenKind::FimAteSeja])?;
        self.expect(TokenKind::FimAteSeja)?;
        Ok(Comando::AteSeja { condicao, corpo, linha })
    }

    /// `repita <bloco> até_que <cond>`. Parênteses na condição são
    /// opcionais (mesma nota de `Self::parse_se`).
    fn parse_repita(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Repita)?;
        let corpo = self.parse_bloco(&[TokenKind::AteQue])?;
        self.expect(TokenKind::AteQue)?;
        let condicao = self.parse_expr()?;
        Ok(Comando::Repita { corpo, condicao, linha })
    }

    /// `execute <bloco> enquanto_for <cond>`. Parênteses na condição são
    /// opcionais (mesma nota de `Self::parse_se`).
    fn parse_execute(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Execute)?;
        let corpo = self.parse_bloco(&[TokenKind::EnquantoFor])?;
        self.expect(TokenKind::EnquantoFor)?;
        let condicao = self.parse_expr()?;
        Ok(Comando::Execute { corpo, condicao, linha })
    }

    /// `laço <bloco_com_saia> fim_laço`. `saia_caso`/`interrompa` são
    /// comandos normais dentro do `corpo`, tratados em [`Self::parse_comando`].
    fn parse_laco(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Laco)?;
        let corpo = self.parse_bloco(&[TokenKind::FimLaco])?;
        self.expect(TokenKind::FimLaco)?;
        Ok(Comando::Laco { corpo, linha })
    }

    /// `para <var> de <ini> até <fim> [passo <passo>] faça <bloco> fim_para`.
    /// `<passo>` pode ser negativo (`passo -1`, via `expr_unario`).
    fn parse_para(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Para)?;
        let variavel = self.expect_identificador()?;
        self.expect(TokenKind::De)?;
        let inicio = self.parse_expr()?;
        self.expect(TokenKind::Ate)?;
        let fim = self.parse_expr()?;
        let passo = if self.check(&TokenKind::Passo) {
            self.avancar();
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Faca)?;
        let corpo = self.parse_bloco(&[TokenKind::FimPara])?;
        self.expect(TokenKind::FimPara)?;
        Ok(Comando::Para { variavel, inicio, fim, passo, corpo, linha })
    }

    /// `saia_caso <cond>` — específico do `laço`.
    /// Parênteses na condição são opcionais (mesma nota de `parse_se`).
    fn parse_saia_caso(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::SaiaCaso)?;
        let condicao = self.parse_expr()?;
        Ok(Comando::SaiaCaso { condicao, linha })
    }

    /// `ir_para RÓTULO`.
    fn parse_ir_para(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::IrPara)?;
        let rotulo = self.expect_identificador()?;
        Ok(Comando::IrPara { rotulo, linha })
    }

    /// `dimensione VAR[<ini1>..<fim1> {, <ini2>..<fim2>}]`.
    fn parse_dimensione(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Dimensione)?;
        let variavel = self.expect_identificador()?;
        self.expect(TokenKind::AbreColchete)?;
        let mut dimensoes = Vec::new();
        loop {
            let inicio = self.parse_expr()?;
            self.expect(TokenKind::PontoPonto)?;
            let fim = self.parse_expr()?;
            dimensoes.push((inicio, fim));
            if self.check(&TokenKind::Virgula) {
                self.avancar();
            } else {
                break;
            }
        }
        self.expect(TokenKind::FechaColchete)?;
        Ok(Comando::Dimensione { variavel, dimensoes, linha })
    }

    // -- Comandos de console — estilo CONIO -----------------------------

    /// `limpar_linha` ou `limpar_linha(<col>)`.
    fn parse_limpar_linha(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::LimparLinha)?;
        let coluna = if self.check(&TokenKind::AbreParen) {
            self.avancar();
            let e = self.parse_expr()?;
            self.expect(TokenKind::FechaParen)?;
            Some(e)
        } else {
            None
        };
        Ok(Comando::LimparLinha { coluna, linha })
    }

    /// `posicionar(<col>, <lin>)`.
    fn parse_posicionar(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::Posicionar)?;
        self.expect(TokenKind::AbreParen)?;
        let coluna = self.parse_expr()?;
        self.expect(TokenKind::Virgula)?;
        let linha_destino = self.parse_expr()?;
        self.expect(TokenKind::FechaParen)?;
        Ok(Comando::Posicionar { coluna, linha_destino, linha })
    }

    /// `cor_fundo(<n>)`.
    fn parse_cor_fundo(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::CorFundo)?;
        self.expect(TokenKind::AbreParen)?;
        let cor = self.parse_expr()?;
        self.expect(TokenKind::FechaParen)?;
        Ok(Comando::CorFundo { cor, linha })
    }

    /// `cor_frente(<n>)`.
    fn parse_cor_frente(&mut self) -> Result<Comando, ErroSintatico> {
        let linha = self.linha_atual();
        self.expect(TokenKind::CorFrente)?;
        self.expect(TokenKind::AbreParen)?;
        let cor = self.parse_expr()?;
        self.expect(TokenKind::FechaParen)?;
        Ok(Comando::CorFrente { cor, linha })
    }

    // =====================================================================================
    // L-values: NOME, NOME.CAMPO, NOME[i], NOME[i,j], ALUNO[I].NOTAS[J]...
    // =====================================================================================

    /// Olha se, depois do identificador `primeiro` (já consumido pelo
    /// chamador), vem `.. IDENTIFICADOR` — reconhecendo a sintaxe de
    /// qualificação de escopo em posição de expressão/lvalue
    /// (`CLS_BASE..OBJETO.CAMPO`, para
    /// desambiguar herança múltipla). Mesmos dois tokens (identificador,
    /// `..`, identificador) usados pela forma de método externo (seção
    /// 10.3), mas em contexto diferente (início de comando/expressão em
    /// vez de início de declaração de topo) — sem ambiguidade real,
    /// já que os dois contextos nunca se confundem no parser.
    /// Retorna `(qualificador_base, nome_real)`.
    fn parse_qualificador_base_opcional(&mut self, primeiro: String) -> Result<(Option<String>, String), ErroSintatico> {
        if self.check(&TokenKind::PontoPonto) {
            self.avancar();
            let nome_real = self.expect_identificador()?;
            Ok((Some(primeiro), nome_real))
        } else {
            Ok((None, primeiro))
        }
    }

    fn parse_lvalue(&mut self) -> Result<LValue, ErroSintatico> {
        let linha = self.linha_atual();
        let primeiro = self.expect_identificador()?;
        let (qualificador_base, nome) = self.parse_qualificador_base_opcional(primeiro)?;
        let acessos = self.parse_acessos()?;
        Ok(LValue { qualificador_base, nome, acessos, linha })
    }

    /// Lê uma sequência de acessos `.CAMPO` e `[i]`/`[i,j]`.
    fn parse_acessos(&mut self) -> Result<Vec<Acesso>, ErroSintatico> {
        let mut acessos = Vec::new();
        loop {
            if self.check(&TokenKind::Ponto) {
                self.avancar();
                let nome = self.expect_identificador()?;
                if self.check(&TokenKind::AbreParen) {
                    // '.MÉTODO(args)' — chamada de método,
                    // não acesso a campo. Sempre exige parênteses, mesmo
                    // sem argumentos (mesma regra de qualquer chamada de
                    // sub-rotina ).
                    let argumentos = self.parse_lista_argumentos()?;
                    acessos.push(Acesso::Metodo { nome, argumentos });
                } else {
                    acessos.push(Acesso::Campo(nome));
                }
            } else if self.check(&TokenKind::AbreColchete) {
                self.avancar();
                let mut indices = vec![self.parse_expr()?];
                while self.check(&TokenKind::Virgula) {
                    self.avancar();
                    indices.push(self.parse_expr()?);
                }
                self.expect(TokenKind::FechaColchete)?;
                acessos.push(Acesso::Indice(indices));
            } else {
                break;
            }
        }
        Ok(acessos)
    }

    // =====================================================================================
    // Expressões — cadeia de precedência
    // =====================================================================================

    fn parse_expr(&mut self) -> Result<Expr, ErroSintatico> {
        self.parse_expr_ou()
    }

    /// Nível 8: `.ou.` e `.xou.` — associativos à esquerda.
    fn parse_expr_ou(&mut self) -> Result<Expr, ErroSintatico> {
        let mut esquerda = self.parse_expr_e()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Ou => OpBinario::Ou,
                TokenKind::Xou => OpBinario::Xou,
                _ => break,
            };
            let linha = self.linha_atual();
            self.avancar();
            let direita = self.parse_expr_e()?;
            esquerda = Expr::Binaria {
                op,
                esquerda: Box::new(esquerda),
                direita: Box::new(direita),
                linha,
            };
        }
        Ok(esquerda)
    }

    /// Nível 7: `.e.` — associativo à esquerda.
    fn parse_expr_e(&mut self) -> Result<Expr, ErroSintatico> {
        let mut esquerda = self.parse_expr_rel()?;
        while self.check(&TokenKind::E) {
            let linha = self.linha_atual();
            self.avancar();
            let direita = self.parse_expr_rel()?;
            esquerda = Expr::Binaria {
                op: OpBinario::E,
                esquerda: Box::new(esquerda),
                direita: Box::new(direita),
                linha,
            };
        }
        Ok(esquerda)
    }

    /// Nível 6: relacionais (`=`, `<>`, `<`, `>`, `<=`, `>=`) — não
    /// associativos (no máximo uma comparação).
    fn parse_expr_rel(&mut self) -> Result<Expr, ErroSintatico> {
        let esquerda = self.parse_expr_add()?;
        let op = match self.peek().kind {
            TokenKind::Igual => OpBinario::Igual,
            TokenKind::Diferente => OpBinario::Diferente,
            TokenKind::Menor => OpBinario::Menor,
            TokenKind::Maior => OpBinario::Maior,
            TokenKind::MenorIgual => OpBinario::MenorIgual,
            TokenKind::MaiorIgual => OpBinario::MaiorIgual,
            _ => return Ok(esquerda),
        };
        let linha = self.linha_atual();
        self.avancar();
        let direita = self.parse_expr_add()?;
        Ok(Expr::Binaria { op, esquerda: Box::new(esquerda), direita: Box::new(direita), linha })
    }

    /// Nível 4: `+` e `-` (binário) — associativos à esquerda. Inclui
    /// concatenação de `cadeia` com `+`, resolvida pelo
    /// verificador semântico/interpretador a partir dos tipos.
    fn parse_expr_add(&mut self) -> Result<Expr, ErroSintatico> {
        let mut esquerda = self.parse_expr_mul()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Mais => OpBinario::Soma,
                TokenKind::Menos => OpBinario::Subtracao,
                _ => break,
            };
            let linha = self.linha_atual();
            self.avancar();
            let direita = self.parse_expr_mul()?;
            esquerda = Expr::Binaria {
                op,
                esquerda: Box::new(esquerda),
                direita: Box::new(direita),
                linha,
            };
        }
        Ok(esquerda)
    }

    /// Nível 3: `*`, `/`, `div`, `mod` — associativos à esquerda, mesma
    /// precedência.
    fn parse_expr_mul(&mut self) -> Result<Expr, ErroSintatico> {
        let mut esquerda = self.parse_expr_unario()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Asterisco => OpBinario::Multiplicacao,
                TokenKind::Barra => OpBinario::Divisao,
                TokenKind::Div => OpBinario::Div,
                TokenKind::Mod => OpBinario::Mod,
                _ => break,
            };
            let linha = self.linha_atual();
            self.avancar();
            let direita = self.parse_expr_unario()?;
            esquerda = Expr::Binaria {
                op,
                esquerda: Box::new(esquerda),
                direita: Box::new(direita),
                linha,
            };
        }
        Ok(esquerda)
    }

    /// Nível 2: `-` (unário) e `.não./.nao.`.
    fn parse_expr_unario(&mut self) -> Result<Expr, ErroSintatico> {
        let linha = self.linha_atual();
        match self.peek().kind {
            TokenKind::Menos => {
                self.avancar();
                let expr = self.parse_expr_pot()?;
                Ok(Expr::Unaria { op: OpUnario::Negativo, expr: Box::new(expr), linha })
            }
            TokenKind::Nao => {
                self.avancar();
                let expr = self.parse_expr_pot()?;
                Ok(Expr::Unaria { op: OpUnario::Nao, expr: Box::new(expr), linha })
            }
            _ => self.parse_expr_pot(),
        }
    }

    /// Nível 1: `^`/`↑` — associativo à direita. O expoente
    /// pode conter um `expr_unario` (ex.: `A ^ -1`).
    fn parse_expr_pot(&mut self) -> Result<Expr, ErroSintatico> {
        let base = self.parse_expr_primaria()?;
        if self.check(&TokenKind::Potencia) {
            let linha = self.linha_atual();
            self.avancar();
            let expoente = self.parse_expr_unario()?;
            Ok(Expr::Binaria {
                op: OpBinario::Potencia,
                esquerda: Box::new(base),
                direita: Box::new(expoente),
                linha,
            })
        } else {
            Ok(base)
        }
    }

    /// Nível 0: literais, variáveis/acessos, chamadas, parênteses e *casts*
    /// (ambas as sintaxes).
    fn parse_expr_primaria(&mut self) -> Result<Expr, ErroSintatico> {
        let linha = self.linha_atual();

        // -- Cast estilo função: inteiro(X), real(X), cadeia(X), caractere(X), lógico(X)
        if let Some(tp) = self.tipo_primitivo_atual() {
            self.avancar();
            self.expect(TokenKind::AbreParen)?;
            let expr = self.parse_expr()?;
            self.expect(TokenKind::FechaParen)?;
            return Ok(Expr::Cast { tipo: tp, expr: Box::new(expr), linha });
        }

        match self.peek().kind.clone() {
            TokenKind::Inteiro(n) => {
                self.avancar();
                Ok(Expr::Inteiro(n))
            }
            TokenKind::Real(n) => {
                self.avancar();
                Ok(Expr::Real(n))
            }
            TokenKind::Texto(s) => {
                self.avancar();
                Ok(Expr::Texto(s))
            }
            TokenKind::Caractere(c) => {
                self.avancar();
                Ok(Expr::Caractere(c))
            }
            TokenKind::Logico(b) => {
                self.avancar();
                Ok(Expr::Logico(b))
            }

            TokenKind::AbreParen => {
                self.avancar();

                // -- Cast estilo C: "(" tipo_primitivo ")" expr_unario
                if let Some(tp) = self.tipo_primitivo_atual() {
                    if self.peek_at(1) == &TokenKind::FechaParen {
                        self.avancar(); // tipo primitivo
                        self.avancar(); // ')'
                        let operando = self.parse_expr_unario()?;
                        return Ok(Expr::Cast { tipo: tp, expr: Box::new(operando), linha });
                    }
                }

                // -- Agrupamento: "(" expr ")"
                let expr = self.parse_expr()?;
                self.expect(TokenKind::FechaParen)?;
                Ok(expr)
            }

            TokenKind::Identificador(primeiro) => {
                self.avancar();
                if self.check(&TokenKind::PontoPonto) {
                    let (qualificador_base, nome) = self.parse_qualificador_base_opcional(primeiro)?;
                    let acessos = self.parse_acessos()?;
                    Ok(Expr::Variavel(LValue { qualificador_base, nome, acessos, linha }))
                } else if self.check(&TokenKind::AbreParen) {
                    let argumentos = self.parse_lista_argumentos()?;
                    Ok(Expr::Chamada { nome: primeiro, argumentos, linha })
                } else {
                    let acessos = self.parse_acessos()?;
                    Ok(Expr::Variavel(LValue { qualificador_base: None, nome: primeiro, acessos, linha }))
                }
            }

            // 'este': só existe dentro do corpo de um
            // método, referenciando a própria instância — sempre seguido
            // de '.CAMPO' ou '.MÉTODO(...)' (nunca chamável diretamente
            // como 'este(...)', diferente de um identificador comum).
            TokenKind::Este => {
                self.avancar();
                let acessos = self.parse_acessos()?;
                Ok(Expr::Variavel(LValue {
                    qualificador_base: None,
                    nome: "este".to_string(),
                    acessos,
                    linha,
                }))
            }

            outro => Err(self.erro(format!(
                "expressão inválida: encontrei '{outro}'. Esperava um número, texto, \
                 valor lógico, variável, chamada de função ou '(' ."
            ))),
        }
    }

    /// Um literal isolado — usado em `const` e `seja`.
    fn parse_literal(&mut self) -> Result<Expr, ErroSintatico> {
        match self.peek().kind.clone() {
            TokenKind::Inteiro(n) => {
                self.avancar();
                Ok(Expr::Inteiro(n))
            }
            TokenKind::Real(n) => {
                self.avancar();
                Ok(Expr::Real(n))
            }
            TokenKind::Texto(s) => {
                self.avancar();
                Ok(Expr::Texto(s))
            }
            TokenKind::Caractere(c) => {
                self.avancar();
                Ok(Expr::Caractere(c))
            }
            TokenKind::Logico(b) => {
                self.avancar();
                Ok(Expr::Logico(b))
            }
            outro => Err(self.erro(format!(
                "esperava um valor literal (número, texto ou valor lógico), \
                 mas encontrei '{outro}'"
            ))),
        }
    }
}

/// Descreve uma sequência de [`Acesso`] em sintaxe PEPPE, para mensagens de
/// erro (ex.: `.NOME` ou `[...]`).
fn descrever_acessos(acessos: &[Acesso]) -> String {
    let mut s = String::new();
    for acesso in acessos {
        match acesso {
            Acesso::Campo(nome) => {
                s.push('.');
                s.push_str(nome);
            }
            Acesso::Indice(_) => {
                s.push_str("[...]");
            }
            Acesso::Metodo { nome, .. } => {
                s.push('.');
                s.push_str(nome);
                s.push_str("(...)");
            }
        }
    }
    s
}

// =====================================================================================
// Testes
// =====================================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenizar;

    /// Tokeniza e analisa `fonte`, retornando a AST do programa. Em caso de
    /// erro, falha o teste mostrando a mensagem de erro completa.
    fn parse(fonte: &str) -> Programa {
        let tokens = tokenizar(fonte).expect("não deveria haver erro léxico");
        match parsear(tokens) {
            Ok(p) => p,
            Err(e) => panic!("erro sintático inesperado: {e}"),
        }
    }

    /// Tokeniza e analisa `fonte`, esperando um erro sintático — retorna o
    /// erro para inspeção.
    fn parse_erro(fonte: &str) -> ErroSintatico {
        let tokens = tokenizar(fonte).expect("não deveria haver erro léxico");
        match parsear(tokens) {
            Ok(_) => panic!("esperava erro sintático, mas o parser teve sucesso"),
            Err(e) => e,
        }
    }

    #[test]
    fn const_tipo_var_com_procedimentos_aninhados() {
        // FIM colide com a palavra-chave 'fim' (case-insensitive).
        // Usa MAXIMO como nome de constante para evitar a colisão.
        let p = parse(r#"programa P
const
  MAXIMO = 10
tipo
  MAT = conjunto [1..MAXIMO] de cadeia
var
  T : mat
  procedimento DOBRA
  var
    I : inteiro
  início
    para I de 1 até MAXIMO passo 1 faça
      leia T[I]
    fim_para
  fim
início
  DOBRA()
fim"#);
        assert_eq!(p.nome, "P");
    }

    #[test]
    fn programa_adicao_numeros() {
        let fonte = r#"programa ADIÇÃO_NÚMEROS
var
  X : inteiro
  A : inteiro
  B : inteiro
início
  leia A
  leia B
  X <- A + B
  escreva X
fim"#;
        let p = parse(fonte);
        assert_eq!(p.nome, "ADIÇÃO_NÚMEROS");
        assert_eq!(p.declaracoes.len(), 3); // três DeclaracaoVar (X, A, B)
        assert_eq!(p.bloco_principal.len(), 4);

        match &p.bloco_principal[2] {
            Comando::Atribuicao { destino, valor, .. } => {
                assert_eq!(destino.nome, "X");
                assert!(matches!(valor, Expr::Binaria { op: OpBinario::Soma, .. }));
            }
            outro => panic!("esperava Atribuicao, encontrei {outro:?}"),
        }
    }

    #[test]
    fn var_com_multiplos_nomes_na_mesma_linha() {
        // "VH, PD, TD, SB, SL : real" — SALÁRIO_PROFESSOR
        let fonte = r#"programa P
var
  HT : inteiro
  VH, PD, TD, SB, SL : real
início
  leia HT, VH, PD
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(d) = &p.declaracoes[1] else {
            panic!("esperava DeclaracaoTopo::Var");
        };
        assert_eq!(d.nomes, vec!["VH", "PD", "TD", "SB", "SL"]);
        assert_eq!(d.tipo, Tipo::Primitivo(TipoPrimitivo::Real));
    }

    #[test]
    fn se_senao_aninhado() {
        // REAJUSTA_SALÁRIO
        let fonte = r#"programa P
var
  SA, NS : real
início
  se (SA < 500) então
    NS <- SA * 1.15
  senão
    se (SA <= 1000) então
      NS <- SA * 1.10
    senão
      NS <- SA * 1.05
    fim_se
  fim_se
fim"#;
        let p = parse(fonte);
        let Comando::Se { entao, senao, .. } = &p.bloco_principal[0] else {
            panic!("esperava Se");
        };
        assert_eq!(entao.len(), 1);
        let senao = senao.as_ref().expect("esperava ramo senão");
        assert_eq!(senao.len(), 1);
        assert!(matches!(senao[0], Comando::Se { .. }));
    }

    #[test]
    fn se_aceita_condicao_sem_parenteses_envolvendo_tudo() {
        // Parênteses na condição de 'se' são opcionais (cada forma é
        // válida e equivalente): com parênteses em cada comparação mas
        // sem um parêntese externo envolvendo a condição inteira, e sem
        // nenhum parêntese.
        let fonte = r#"programa P
var
  N : inteiro
início
  se (N >= 20) .e. (N <= 90) então
    N <- 1
  fim_se
  se N >= 20 .e. N <= 90 então
    N <- 2
  fim_se
fim"#;
        let p = parse(fonte);
        assert!(matches!(p.bloco_principal[0], Comando::Se { .. }));
        assert!(matches!(p.bloco_principal[1], Comando::Se { .. }));
    }

    #[test]
    fn enquanto_aceita_condicao_sem_parenteses_envolvendo_tudo() {
        let fonte = r#"programa P
var
  I : inteiro
início
  enquanto I <= 5 faça
    I <- I + 1
  fim_enquanto
fim"#;
        let p = parse(fonte);
        assert!(matches!(p.bloco_principal[0], Comando::Enquanto { .. }));
    }

    #[test]
    fn enquanto_e_para_com_passo() {
        let fonte = r#"programa P
var
  I, N, R, TOPO : inteiro
início
  I <- 1
  enquanto (I <= 5) faça
    leia N
    I <- I + 1
  fim_enquanto

  para I de 1 até 10 passo 1 faça
    R <- N * 3
  fim_para

  para I de TOPO até 1 passo -1 faça
    escreva I
  fim_para
fim"#;
        let p = parse(fonte);
        assert!(matches!(p.bloco_principal[1], Comando::Enquanto { .. }));

        let Comando::Para { passo, .. } = &p.bloco_principal[2] else {
            panic!("esperava Para");
        };
        assert_eq!(*passo, Some(Expr::Inteiro(1)));

        let Comando::Para { passo, .. } = &p.bloco_principal[3] else {
            panic!("esperava Para");
        };
        // passo -1: Unaria(Negativo, Inteiro(1))
        match passo {
            Some(Expr::Unaria { op: OpUnario::Negativo, expr, .. }) => {
                assert_eq!(**expr, Expr::Inteiro(1));
            }
            outro => panic!("esperava Unaria(Negativo, Inteiro(1)), encontrei {outro:?}"),
        }
    }

    #[test]
    fn caso_com_e_sem_senao() {
        let fonte = r#"programa P
var
  N : inteiro
início
  caso N
    seja 1 faça
      escreva "um"
    seja 2 faça
      escreva "dois"
  fim_caso

  caso N
    seja 1 faça
      escreva "um"
    senão
      escreva "outro"
  fim_caso
fim"#;
        let p = parse(fonte);

        let Comando::Caso { ramos, senao, .. } = &p.bloco_principal[0] else {
            panic!("esperava Caso");
        };
        assert_eq!(ramos.len(), 2);
        assert_eq!(senao, &None);

        let Comando::Caso { ramos, senao, .. } = &p.bloco_principal[1] else {
            panic!("esperava Caso");
        };
        assert_eq!(ramos.len(), 1);
        assert!(senao.is_some());
    }

    #[test]
    fn caso_com_chamada_de_procedimento_em_seja_faca() {
        // Padrão das calculadoras do material de origem: 'seja N faça
        // ROTINA()' — agora exige parênteses.
        let fonte = r#"programa P

  procedimento ROTSOMA
  início
  fim

var
  OPCAO : inteiro
início
  caso OPCAO
    seja 1 faça ROTSOMA()
  fim_caso
fim"#;
        let p = parse(fonte);
        let Comando::Caso { ramos, .. } = &p.bloco_principal[0] else {
            panic!("esperava Caso");
        };
        let Comando::ChamadaProcedimento { nome, argumentos, .. } = &ramos[0].corpo[0] else {
            panic!("esperava ChamadaProcedimento");
        };
        assert_eq!(nome, "ROTSOMA");
        assert!(argumentos.is_empty());
    }

    #[test]
    fn escreva_com_especificadores_de_formatacao() {
        // escreva R : 8 : 2   /   escreva N : 8   /   escreva X
        let fonte = r#"programa P
var
  R : real
  N, X : inteiro
início
  escreva R : 8 : 2
  escreva N : 8
  escreva X
fim"#;
        let p = parse(fonte);

        let Comando::Escreva { itens, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva");
        };
        assert_eq!(itens[0].largura, Some(Expr::Inteiro(8)));
        assert_eq!(itens[0].decimais, Some(Expr::Inteiro(2)));

        let Comando::Escreva { itens, .. } = &p.bloco_principal[1] else {
            panic!("esperava Escreva");
        };
        assert_eq!(itens[0].largura, Some(Expr::Inteiro(8)));
        assert_eq!(itens[0].decimais, None);

        let Comando::Escreva { itens, .. } = &p.bloco_principal[2] else {
            panic!("esperava Escreva");
        };
        assert_eq!(itens[0].largura, None);
    }

    #[test]
    fn escreva_ln_com_itens_marca_quebra_linha() {
        let fonte = r#"programa P
var
  X : inteiro
início
  escreva_ln X : 8 : 2
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { itens, quebra_linha, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva");
        };
        assert!(*quebra_linha);
        assert_eq!(itens.len(), 1);
        assert_eq!(itens[0].largura, Some(Expr::Inteiro(8)));
        assert_eq!(itens[0].decimais, Some(Expr::Inteiro(2)));
    }

    #[test]
    fn escreva_sem_quebra_linha_marca_quebra_linha_falso() {
        let fonte = r#"programa P
var
  X : inteiro
início
  escreva X
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { quebra_linha, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva");
        };
        assert!(!*quebra_linha);
    }

    #[test]
    fn escreva_ln_sozinho_sem_itens_antes_de_fim_se() {
        // 'escreva_ln' seguido imediatamente de 'fim_se' — nenhum item,
        // apenas a quebra de linha.
        let fonte = r#"programa P
var
  X : inteiro
início
  se (X > 0) então
    escreva_ln
  fim_se
fim"#;
        let p = parse(fonte);
        let Comando::Se { entao, .. } = &p.bloco_principal[0] else {
            panic!("esperava Se");
        };
        let Comando::Escreva { itens, quebra_linha, .. } = &entao[0] else {
            panic!("esperava Escreva");
        };
        assert!(*quebra_linha);
        assert!(itens.is_empty());
    }

    #[test]
    fn escreva_ln_sozinho_antes_de_fim_do_programa() {
        let fonte = r#"programa P
início
  escreva_ln
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { itens, quebra_linha, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva");
        };
        assert!(*quebra_linha);
        assert!(itens.is_empty());
    }

    #[test]
    fn escreva_ln_com_multiplos_itens() {
        let fonte = r#"programa P
var
  A, B, C : inteiro
início
  escreva_ln A, B, C
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { itens, quebra_linha, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva");
        };
        assert!(*quebra_linha);
        assert_eq!(itens.len(), 3);
    }

    #[test]
    fn precedencia_aritmetica() {
        // X <- 2 + 3 * 4   ->   Soma(2, Multiplicacao(3, 4))
        let fonte = r#"programa P
var
  X : inteiro
início
  X <- 2 + 3 * 4
fim"#;
        let p = parse(fonte);
        let Comando::Atribuicao { valor, .. } = &p.bloco_principal[0] else {
            panic!("esperava Atribuicao");
        };
        let Expr::Binaria { op: OpBinario::Soma, esquerda, direita, .. } = valor else {
            panic!("esperava Binaria(Soma)");
        };
        assert_eq!(**esquerda, Expr::Inteiro(2));
        assert!(matches!(**direita, Expr::Binaria { op: OpBinario::Multiplicacao, .. }));
    }

    #[test]
    fn precedencia_potencia_associativa_a_direita() {
        // A <- 2 ^ 3 ^ 2   ->   Potencia(2, Potencia(3, 2))  (assoc. à direita)
        let fonte = r#"programa P
var
  A : inteiro
início
  A <- 2 ^ 3 ^ 2
fim"#;
        let p = parse(fonte);
        let Comando::Atribuicao { valor, .. } = &p.bloco_principal[0] else {
            panic!("esperava Atribuicao");
        };
        let Expr::Binaria { op: OpBinario::Potencia, esquerda, direita, .. } = valor else {
            panic!("esperava Binaria(Potencia)");
        };
        assert_eq!(**esquerda, Expr::Inteiro(2));
        assert!(matches!(**direita, Expr::Binaria { op: OpBinario::Potencia, .. }));
    }

    #[test]
    fn operadores_logicos_e_relacionais() {
        // se ((A >= 20) .e. (A <= 90) .ou. .não. B .xou. C) então
        //
        // Nota: 'se' exige que TODA a condição esteja entre um único par de
        // parênteses — os parênteses em '(A >= 20)' e '(A <= 90)'
        // são agrupamentos internos opcionais, não a delimitação do 'se'.
        let fonte = r#"programa P
var
  A : inteiro
  B, C : lógico
início
  se ((A >= 20) .e. (A <= 90) .ou. .não. B .xou. C) então
    escreva A
  fim_se
fim"#;
        let p = parse(fonte);
        let Comando::Se { condicao, .. } = &p.bloco_principal[0] else {
            panic!("esperava Se");
        };
        // O nível mais externo deve ser .ou./.xou. (precedência mais baixa)
        assert!(matches!(
            condicao,
            Expr::Binaria { op: OpBinario::Ou, .. } | Expr::Binaria { op: OpBinario::Xou, .. }
        ));
    }

    #[test]
    fn tipo_registro_e_conjunto() {
        let fonte = r#"programa P
tipo
  BIMESTRE = conjunto [1..4] de real
  CAD_ALUNO = registro
                NOME  : cadeia
                TURMA : caractere
                NOTAS : bimestre
              fim_registro
var
  ALUNO : cad_aluno
início
  leia ALUNO.NOME
fim"#;
        let p = parse(fonte);

        let DeclaracaoTopo::Tipo(d) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::Tipo (BIMESTRE)");
        };
        assert_eq!(d.nome, "BIMESTRE");
        match &d.definicao {
            Tipo::Conjunto { dimensoes, elemento } => {
                assert_eq!(dimensoes.len(), 1);
                assert_eq!(**elemento, Tipo::Primitivo(TipoPrimitivo::Real));
            }
            outro => panic!("esperava Tipo::Conjunto, encontrei {outro:?}"),
        }

        let DeclaracaoTopo::Tipo(d) = &p.declaracoes[1] else {
            panic!("esperava DeclaracaoTopo::Tipo (CAD_ALUNO)");
        };
        assert_eq!(d.nome, "CAD_ALUNO");
        match &d.definicao {
            Tipo::Registro(campos) => {
                assert_eq!(campos.len(), 3);
                assert_eq!(campos[2].tipo, Tipo::Nomeado("bimestre".into()));
            }
            outro => panic!("esperava Tipo::Registro, encontrei {outro:?}"),
        }

        // var ALUNO : cad_aluno  (case-insensitive — resolvido pelo checker)
        let DeclaracaoTopo::Var(d) = &p.declaracoes[2] else {
            panic!("esperava DeclaracaoTopo::Var (ALUNO)");
        };
        assert_eq!(d.tipo, Tipo::Nomeado("cad_aluno".into()));
    }

    #[test]
    fn conjunto_dinamico_e_dimensione() {
        // MATRIZ_DINÂMICA
        let fonte = r#"programa MATRIZ_DINÂMICA
var
  I, N : inteiro
  A : conjunto [] de cadeia
início
  leia N
  dimensione A[1..N]
  para I de 1 até N passo 1 faça
    leia A[I]
  fim_para
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(d) = &p.declaracoes[1] else {
            panic!("esperava DeclaracaoTopo::Var (A)");
        };
        match &d.tipo {
            Tipo::Conjunto { dimensoes, .. } => assert_eq!(dimensoes, &vec![None]),
            outro => panic!("esperava Tipo::Conjunto, encontrei {outro:?}"),
        }

        let Comando::Dimensione { variavel, dimensoes, .. } = &p.bloco_principal[1] else {
            panic!("esperava Dimensione");
        };
        assert_eq!(variavel, "A");
        assert_eq!(dimensoes.len(), 1);
    }

    #[test]
    fn dimensione_matriz_2d() {
        // Questão #7: sintaxe para 'dimensione' com mais de uma
        // dimensão — vírgula separa pares <início>..<fim>, consistente com
        // a declaração de tipo 'conjunto [1..N, 1..M] de <tipo>'.
        let fonte = r#"programa MATRIZ_2D
var
  L, C : inteiro
  M : conjunto [1..10, 1..10] de inteiro
início
  leia L
  leia C
  dimensione M[1..L, 1..C]
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(d) = &p.declaracoes[1] else {
            panic!("esperava DeclaracaoTopo::Var (M)");
        };
        match &d.tipo {
            Tipo::Conjunto { dimensoes, .. } => assert_eq!(dimensoes.len(), 2),
            outro => panic!("esperava Tipo::Conjunto, encontrei {outro:?}"),
        }

        let Comando::Dimensione { variavel, dimensoes, .. } = &p.bloco_principal[2] else {
            panic!("esperava Dimensione");
        };
        assert_eq!(variavel, "M");
        assert_eq!(dimensoes.len(), 2);
    }

    #[test]
    fn conjunto_duas_dimensoes_dinamicas() {
        // 'conjunto [,] de <tipo>' — 2 dimensões dinâmicas (uma vírgula
        // extra por dimensão, conforme decidido para a questão #7).
        let fonte = r#"programa P
var
  M : conjunto [,] de inteiro
início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(d) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::Var (M)");
        };
        match &d.tipo {
            Tipo::Conjunto { dimensoes, .. } => assert_eq!(dimensoes, &vec![None, None]),
            outro => panic!("esperava Tipo::Conjunto, encontrei {outro:?}"),
        }
    }

    #[test]
    fn conjunto_tres_dimensoes_dinamicas() {
        // 'conjunto [,,] de <tipo>' — 3 dimensões dinâmicas.
        let fonte = r#"programa P
var
  CUBO : conjunto [,,] de real
início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(d) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::Var (CUBO)");
        };
        match &d.tipo {
            Tipo::Conjunto { dimensoes, .. } => assert_eq!(dimensoes, &vec![None, None, None]),
            outro => panic!("esperava Tipo::Conjunto, encontrei {outro:?}"),
        }
    }

    #[test]
    fn conjunto_dimensoes_mistas_estatica_e_dinamica() {
        // 'conjunto [1..10,] de <tipo>' — primeira dimensão fixa em 10,
        // segunda a ser dimensionada depois via 'dimensione'.
        let fonte = r#"programa P
var
  M : conjunto [1..10,] de cadeia
início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(d) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::Var (M)");
        };
        match &d.tipo {
            Tipo::Conjunto { dimensoes, .. } => {
                assert_eq!(dimensoes.len(), 2);
                assert!(dimensoes[0].is_some());
                assert!(dimensoes[1].is_none());
            }
            outro => panic!("esperava Tipo::Conjunto, encontrei {outro:?}"),
        }
    }

    #[test]
    fn procedimento_com_parametros_padrao_a() {
        // CALC_FAT_V2 (marcador de referência é 'ref')
        let fonte = r#"programa P

  procedimento FATORIAL(N : inteiro; ref FAT : inteiro)
  var
    I : inteiro
  início
    para I de 1 até N passo 1 faça
      FAT <- FAT * I
    fim_para
  fim

var
  LIMITE, RESP : inteiro
início
  RESP <- 1
  FATORIAL(LIMITE, RESP)
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::SubRotina(sub) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::SubRotina");
        };
        assert_eq!(sub.categoria, CategoriaSubRotina::Procedimento);
        assert_eq!(sub.nome, "FATORIAL");
        assert_eq!(sub.parametros.len(), 2);
        assert_eq!(sub.parametros[0].nomes, vec!["N"]);
        assert!(!sub.parametros[0].por_referencia);
        assert_eq!(sub.parametros[1].nomes, vec!["FAT"]);
        assert!(sub.parametros[1].por_referencia);
        assert_eq!(sub.declaracoes_locais.len(), 1); // var I : inteiro
        assert_eq!(sub.corpo.len(), 1); // para ... fim_para

        // Chamada no bloco principal
        assert!(matches!(p.bloco_principal[1], Comando::ChamadaProcedimento { .. }));
    }

    #[test]
    fn parametros_padrao_b_produz_erro_didatico() {
        // Padrão B (erro do material-fonte): "X, Y : real, OPERADOR : caractere"
        let fonte = r#"programa P
  função CALCULO(X, Y : real, OPERADOR : caractere) : real
  início
    CALCULO <- X + Y
  fim
var
  Z : real
início
  Z <- 1
fim"#;
        let erro = parse_erro(fonte);
        assert!(erro.mensagem.contains("';'"));
        assert!(erro.mensagem.contains("','"));
    }

    #[test]
    fn chamada_de_procedimento_sem_parenteses_e_erro_didatico() {
        // Decisão: '()' é sempre obrigatório em chamadas, mesmo sem
        // argumentos — revoga a permissividade anterior de aceitar
        // 'TROCA' sozinho, sem parênteses, como comando.
        let fonte = r#"programa P

  procedimento TROCA
  início
  fim

início
  TROCA
fim"#;
        let erro = parse_erro(fonte);
        assert!(erro.mensagem.contains("TROCA"));
    }

    #[test]
    fn chamada_de_procedimento_sem_argumentos_exige_parenteses_vazios() {
        let fonte = r#"programa P

  procedimento TROCA
  início
  fim

início
  TROCA()
fim"#;
        let p = parse(fonte);
        let Comando::ChamadaProcedimento { nome, argumentos, .. } = &p.bloco_principal[0] else {
            panic!("esperava ChamadaProcedimento");
        };
        assert_eq!(nome, "TROCA");
        assert!(argumentos.is_empty());
    }

    #[test]
    fn marcador_ref_para_passagem_por_referencia() {
        // 'ref' marca passagem por referência — mais
        // curto que 'referencia'/'var', sem colidir com a seção 'var' de
        // declarações.
        let fonte = r#"programa P
  procedimento DOBRA(ref X : inteiro; ref Y : real)
  início
    X <- X * 2
    Y <- Y * 2.0
  fim
var
  A : inteiro
  B : real
início
  DOBRA(A, B)
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::SubRotina(sub) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::SubRotina");
        };
        assert_eq!(sub.parametros.len(), 2);
        assert!(sub.parametros[0].por_referencia);
        assert!(sub.parametros[1].por_referencia);
    }

    #[test]
    fn marcador_vlr_e_opcional_e_redundante() {
        // 'vlr' marca passagem por valor explicitamente, mas é
        // puramente redundante — 'vlr X : inteiro' e 'X : inteiro' devem
        // produzir exatamente o mesmo Parametro (por_referencia: false).
        let fonte = r#"programa P
  procedimento MISTO(vlr A : inteiro; B : inteiro; ref C : real)
  início
  fim
início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::SubRotina(sub) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::SubRotina");
        };
        assert_eq!(sub.parametros.len(), 3);
        assert!(!sub.parametros[0].por_referencia); // 'vlr A'
        assert!(!sub.parametros[1].por_referencia); // 'B' (sem marcador)
        assert!(sub.parametros[2].por_referencia); // 'ref C'

        // Os dois primeiros parâmetros devem ser estruturalmente idênticos
        // a menos do nome — 'vlr' não deixou nenhuma marca residual.
        assert_eq!(sub.parametros[0].tipo, sub.parametros[1].tipo);
        assert_eq!(sub.parametros[0].por_referencia, sub.parametros[1].por_referencia);
    }

    #[test]
    fn var_aceito_como_sinonimo_de_ref_em_parametros() {
        // 'var' dentro da lista de parâmetros é sinônimo de 'ref' (estilo
        // Pascal) — deve ser aceito e produzir por_referencia: true.
        let p = parse(
            r#"programa P
  procedimento TROCA(var A, B : inteiro)
  início
  fim
início
fim"#,
        );
        let DeclaracaoTopo::SubRotina(sub) = &p.declaracoes[0] else {
            panic!("esperava SubRotina");
        };
        assert!(sub.parametros[0].por_referencia, "var deve ser por referência");
    }

    #[test]
    fn funcao_com_retorno_e_chamada() {
        // CALC_FAT_V3
        let fonte = r#"programa P

  função FATORIAL(N : inteiro) : inteiro
  var
    I, FAT : inteiro
  início
    FAT <- 1
    para I de 1 até N passo 1 faça
      FAT <- FAT * I
    fim_para
    FATORIAL <- FAT
  fim

var
  LIMITE : inteiro
início
  leia LIMITE
  escreva FATORIAL(LIMITE)
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::SubRotina(sub) = &p.declaracoes[0] else {
            panic!("esperava DeclaracaoTopo::SubRotina");
        };
        assert_eq!(sub.categoria, CategoriaSubRotina::Funcao);
        assert_eq!(sub.tipo_retorno, Some(Tipo::Primitivo(TipoPrimitivo::Inteiro)));

        // 'FATORIAL <- FAT' é uma Atribuicao normal (checker valida depois).
        let ultimo = sub.corpo.last().unwrap();
        assert!(matches!(ultimo, Comando::Atribuicao { .. }));

        // 'escreva FATORIAL(LIMITE)' -> Expr::Chamada dentro de ItemEscreva
        let Comando::Escreva { itens, .. } = &p.bloco_principal[1] else {
            panic!("esperava Escreva");
        };
        assert!(matches!(itens[0].expressao, Expr::Chamada { .. }));
    }

    #[test]
    fn cast_estilo_funcao_e_estilo_c() {
        let fonte = r#"programa P
var
  A : inteiro
  B : real
início
  B <- 3.14
  A <- inteiro(B)
  A <- (inteiro) B
fim"#;
        let p = parse(fonte);

        let Comando::Atribuicao { valor, .. } = &p.bloco_principal[1] else {
            panic!("esperava Atribuicao");
        };
        assert!(matches!(valor, Expr::Cast { tipo: TipoPrimitivo::Inteiro, .. }));

        let Comando::Atribuicao { valor, .. } = &p.bloco_principal[2] else {
            panic!("esperava Atribuicao");
        };
        assert!(matches!(valor, Expr::Cast { tipo: TipoPrimitivo::Inteiro, .. }));
    }

    #[test]
    fn rotulo_ir_para_interrompa_saia_caso() {
        let fonte = r#"programa P
var
  I, N, R : inteiro
início
  I <- 1
  laço
    leia N
    R <- N * 3
    saia_caso (I > 4)
    interrompa
  fim_laço

  INICIO_DO_LACO:
    I <- I + 1
    ir_para INICIO_DO_LACO
  FIM_DO_LACO:
fim"#;
        let p = parse(fonte);

        let Comando::Laco { corpo, .. } = &p.bloco_principal[1] else {
            panic!("esperava Laco");
        };
        assert!(matches!(corpo[2], Comando::SaiaCaso { .. }));
        assert!(matches!(corpo[3], Comando::Interrompa { .. }));

        assert!(matches!(p.bloco_principal[2], Comando::Rotulo { .. }));
        assert!(matches!(p.bloco_principal[4], Comando::IrPara { .. }));
        assert!(matches!(p.bloco_principal[5], Comando::Rotulo { .. }));
    }

    #[test]
    fn comandos_conio() {
        let fonte = r#"programa P
var
  SENHA : cadeia
início
  limpar
  posicionar(10, 5)
  cor_fundo(1)
  cor_frente(15)
  escreva "Senha: "
  leia_seco SENHA
  limpar_linha
  limpar_linha(5)
  escreva "Pressione <Enter> para continuar..."
  pausa
fim"#;
        let p = parse(fonte);
        assert!(matches!(p.bloco_principal[0], Comando::Limpar { .. }));
        assert!(matches!(p.bloco_principal[1], Comando::Posicionar { .. }));
        assert!(matches!(p.bloco_principal[2], Comando::CorFundo { .. }));
        assert!(matches!(p.bloco_principal[3], Comando::CorFrente { .. }));
        assert!(matches!(p.bloco_principal[5], Comando::LeiaSeco { .. }));

        let Comando::LimparLinha { coluna, .. } = &p.bloco_principal[6] else {
            panic!("esperava LimparLinha");
        };
        assert_eq!(*coluna, None);

        let Comando::LimparLinha { coluna, .. } = &p.bloco_principal[7] else {
            panic!("esperava LimparLinha");
        };
        assert_eq!(*coluna, Some(Expr::Inteiro(5)));

        assert!(matches!(p.bloco_principal[9], Comando::Pausa { .. }));
    }

    #[test]
    fn erro_mostra_token_esperado_em_sintaxe_peppe() {
        // Falta o 'fim_se'
        let fonte = r#"programa P
var
  X : inteiro
início
  se (X > 0) então
    escreva X
fim"#;
        let erro = parse_erro(fonte);
        assert!(erro.mensagem.contains("fim_se"));
    }

    #[test]
    fn acesso_encadeado_em_atribuicao() {
        // ALUNO[I].NOTAS[J] <- X
        let fonte = r#"programa P
tipo
  CAD = registro
          NOTAS : conjunto [1..4] de real
        fim_registro
var
  ALUNO : conjunto [1..8] de cad
  I, J : inteiro
  X : real
início
  ALUNO[I].NOTAS[J] <- X
fim"#;
        let p = parse(fonte);
        let Comando::Atribuicao { destino, .. } = &p.bloco_principal[0] else {
            panic!("esperava Atribuicao");
        };
        assert_eq!(destino.nome, "ALUNO");
        assert_eq!(destino.acessos.len(), 3);
        assert!(matches!(destino.acessos[0], Acesso::Indice(_)));
        assert!(matches!(destino.acessos[1], Acesso::Campo(_)));
        assert!(matches!(destino.acessos[2], Acesso::Indice(_)));
    }

    // =====================================================================================
    // Programação Orientada a Objetos: classe sem herança
    // =====================================================================================

    #[test]
    fn classe_simples_so_campos() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
              NOTAS : conjunto [1..4] de real
          fim_classe

objeto
  ESTUDANTE : Aluno

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t) = &p.declaracoes[0] else { panic!("esperava Tipo") };
        let Tipo::Classe { heranca, membros } = &t.definicao else { panic!("esperava Classe") };
        assert_eq!(heranca, &Vec::<String>::new());
        assert_eq!(membros.len(), 2);
        assert!(matches!(membros[0].visibilidade, Visibilidade::Publica));
        let ItemClasse::Campo(campo) = &membros[0].item else { panic!("esperava Campo") };
        assert_eq!(campo.nomes, vec!["NOME".to_string()]);

        let DeclaracaoTopo::Var(v) = &p.declaracoes[1] else { panic!("esperava Var (objeto)") };
        assert_eq!(v.nomes, vec!["ESTUDANTE".to_string()]);
        assert_eq!(v.tipo, Tipo::Nomeado("Aluno".to_string()));
    }

    #[test]
    fn classe_com_metodo_interno() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
              NOTAS : conjunto [1..4] de real
              MÉDIA : real
              função CALCMÉDIA() : real
              var
                I : inteiro
                SOMA : real
              início
                SOMA <- 0
                MÉDIA <- SOMA
                CALCMÉDIA <- MÉDIA
              fim
          fim_classe

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t) = &p.declaracoes[0] else { panic!("esperava Tipo") };
        let Tipo::Classe { membros, .. } = &t.definicao else { panic!("esperava Classe") };
        // 3 campos + 1 método interno
        assert_eq!(membros.len(), 4);
        let ItemClasse::MetodoInterno(sub, modificador) = &membros[3].item else {
            panic!("esperava MetodoInterno")
        };
        assert_eq!(sub.nome, "CALCMÉDIA");
        assert_eq!(sub.categoria, CategoriaSubRotina::Funcao);
        assert_eq!(sub.corpo.len(), 3);
        assert_eq!(*modificador, Modificador::Nenhum);
    }

    #[test]
    fn classe_com_assinatura_e_metodo_externo() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
              função CALCMÉDIA : real
          fim_classe

  função Aluno..CALCMÉDIA() : real
  início
    CALCMÉDIA <- 0
  fim

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t) = &p.declaracoes[0] else { panic!("esperava Tipo") };
        let Tipo::Classe { membros, .. } = &t.definicao else { panic!("esperava Classe") };
        assert!(matches!(&membros[1].item, ItemClasse::AssinaturaMetodo { .. }));

        let DeclaracaoTopo::MetodoExterno { classe, metodo } = &p.declaracoes[1] else {
            panic!("esperava MetodoExterno")
        };
        assert_eq!(classe, "Aluno");
        assert_eq!(metodo.nome, "CALCMÉDIA");
    }

    #[test]
    fn classe_com_heranca_simples() {
        let fonte = r#"programa P
tipo
  Sala = classe
           seção_pública
             CAPACIDADE : inteiro
         fim_classe

  Aluno = classe herança de Sala
            seção_pública
              NOME : cadeia
          fim_classe

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t) = &p.declaracoes[1] else { panic!("esperava Tipo") };
        let Tipo::Classe { heranca, .. } = &t.definicao else { panic!("esperava Classe") };
        assert_eq!(heranca, &vec!["Sala".to_string()]);
    }

    #[test]
    fn classe_com_heranca_multipla() {
        // Exemplo de referência do autor: uma
        // classe derivada de duas bases diretas, cada uma repetindo a
        // palavra 'de' depois da vírgula.
        let fonte = r#"programa P
tipo
  CLS_SALA = classe
               seção_protegida
                 SALA : inteiro
             fim_classe

  CLS_TURMA = classe
                seção_protegida
                  TURMA : caractere
              fim_classe

  CLS_ALUNO = classe herança de CLS_SALA, de CLS_TURMA
                seção_pública
                  NOME : cadeia
              fim_classe

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t) = &p.declaracoes[2] else { panic!("esperava Tipo") };
        let Tipo::Classe { heranca, .. } = &t.definicao else { panic!("esperava Classe") };
        assert_eq!(heranca, &vec!["CLS_SALA".to_string(), "CLS_TURMA".to_string()]);
    }

    #[test]
    fn acesso_a_campo_de_objeto_em_expressao() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.NOME
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { itens, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva")
        };
        let Expr::Variavel(lvalue) = &itens[0].expressao else { panic!("esperava Variavel") };
        assert_eq!(lvalue.nome, "ESTUDANTE");
        assert_eq!(lvalue.acessos, vec![Acesso::Campo("NOME".to_string())]);
    }

    #[test]
    fn qualificador_de_base_em_expressao_e_reconhecido() {
        // Herança múltipla: 'CLS_BASE..OBJETO.CAMPO'
        // desambigua de qual base vem o acesso. Aqui só testamos o
        // parsing — a resolução semântica de fato é responsabilidade do
        // checker.
        let fonte = r#"programa P
tipo
  CLS_SALA = classe
               seção_protegida
                 SALA : inteiro
             fim_classe

  CLS_ALUNO = classe herança de CLS_SALA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO

início
  escreva CLS_SALA..ALUNO.SALA
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { itens, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva")
        };
        let Expr::Variavel(lvalue) = &itens[0].expressao else { panic!("esperava Variavel") };
        assert_eq!(lvalue.qualificador_base, Some("CLS_SALA".to_string()));
        assert_eq!(lvalue.nome, "ALUNO");
        assert_eq!(lvalue.acessos, vec![Acesso::Campo("SALA".to_string())]);
    }

    #[test]
    fn qualificador_de_base_em_atribuicao_e_reconhecido() {
        let fonte = r#"programa P
tipo
  CLS_SALA = classe
               seção_protegida
                 SALA : inteiro
             fim_classe

  CLS_ALUNO = classe herança de CLS_SALA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO

início
  CLS_SALA..ALUNO.SALA <- 7
fim"#;
        let p = parse(fonte);
        let Comando::Atribuicao { destino, .. } = &p.bloco_principal[0] else {
            panic!("esperava Atribuicao")
        };
        assert_eq!(destino.qualificador_base, Some("CLS_SALA".to_string()));
        assert_eq!(destino.nome, "ALUNO");
        assert_eq!(destino.acessos, vec![Acesso::Campo("SALA".to_string())]);
    }

    #[test]
    fn lvalue_sem_qualificador_tem_qualificador_base_none() {
        let fonte = r#"programa P
var
  X : inteiro
início
  X <- 5
fim"#;
        let p = parse(fonte);
        let Comando::Atribuicao { destino, .. } = &p.bloco_principal[0] else {
            panic!("esperava Atribuicao")
        };
        assert_eq!(destino.qualificador_base, None);
    }

    #[test]
    fn chamada_de_metodo_sem_argumentos_como_comando() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              função CALCMÉDIA : real
          fim_classe

  função Aluno..CALCMÉDIA() : real
  início
    CALCMÉDIA <- 0
  fim

objeto
  ESTUDANTE : Aluno

início
  ESTUDANTE.CALCMÉDIA()
fim"#;
        let p = parse(fonte);
        let Comando::ChamadaMetodo { alvo, .. } = &p.bloco_principal[0] else {
            panic!("esperava ChamadaMetodo")
        };
        assert_eq!(alvo.nome, "ESTUDANTE");
        assert_eq!(alvo.acessos.len(), 1);
        let Acesso::Metodo { nome, argumentos } = &alvo.acessos[0] else {
            panic!("esperava Acesso::Metodo")
        };
        assert_eq!(nome, "CALCMÉDIA");
        assert!(argumentos.is_empty());
    }

    #[test]
    fn chamada_de_metodo_com_argumentos_em_expressao() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              função PEGANOTA(POS : inteiro) : real
          fim_classe

  função Aluno..PEGANOTA(POS : inteiro) : real
  início
    PEGANOTA <- 0
  fim

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.PEGANOTA(1)
fim"#;
        let p = parse(fonte);
        let Comando::Escreva { itens, .. } = &p.bloco_principal[0] else {
            panic!("esperava Escreva")
        };
        let Expr::Variavel(lvalue) = &itens[0].expressao else { panic!("esperava Variavel") };
        let Acesso::Metodo { nome, argumentos } = &lvalue.acessos[0] else {
            panic!("esperava Acesso::Metodo")
        };
        assert_eq!(nome, "PEGANOTA");
        assert_eq!(argumentos.len(), 1);
    }

    #[test]
    fn objeto_e_var_produzem_a_mesma_representacao_na_ast() {
        let fonte = r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  A : Aluno

var
  B : Aluno

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Var(va) = &p.declaracoes[1] else { panic!("esperava Var") };
        let DeclaracaoTopo::Var(vb) = &p.declaracoes[2] else { panic!("esperava Var") };
        assert_eq!(va.tipo, vb.tipo);
    }

    #[test]
    fn tipo_funcao_com_parametros() {
        // Seção 10.5.3: 'tipo NOME = função(tipo1, tipo2, ...)' fixa só
        // os tipos de parâmetro; uma variável usa o nome do alias.
        let fonte = r#"programa P
tipo
  FUNC1 = função(inteiro)

var
  RESPOSTA : FUNC1

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t) = &p.declaracoes[0] else { panic!("esperava Tipo") };
        assert_eq!(t.nome, "FUNC1");
        assert_eq!(t.definicao, Tipo::Funcao { parametros: vec![Tipo::Primitivo(TipoPrimitivo::Inteiro)] });
        let DeclaracaoTopo::Var(v) = &p.declaracoes[1] else { panic!("esperava Var") };
        assert_eq!(v.tipo, Tipo::Nomeado("FUNC1".to_string()));
    }

    #[test]
    fn tipo_funcao_com_multiplos_parametros_e_sem_parametros() {
        let fonte = r#"programa P
tipo
  FUNC2 = função(real, real)
  FUNC3 = função()

início
fim"#;
        let p = parse(fonte);
        let DeclaracaoTopo::Tipo(t2) = &p.declaracoes[0] else { panic!("esperava Tipo") };
        assert_eq!(
            t2.definicao,
            Tipo::Funcao {
                parametros: vec![Tipo::Primitivo(TipoPrimitivo::Real), Tipo::Primitivo(TipoPrimitivo::Real)]
            }
        );
        let DeclaracaoTopo::Tipo(t3) = &p.declaracoes[1] else { panic!("esperava Tipo") };
        assert_eq!(t3.definicao, Tipo::Funcao { parametros: vec![] });
    }
}
