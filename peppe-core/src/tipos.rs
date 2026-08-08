//! Tipos *resolvidos* e regras de compatibilidade/coerção (seção 10.5),
//! usados pelo verificador semântico (`checker.rs`, seção 15).
//!
//! A AST (`ast::Tipo`) pode conter [`ast::Tipo::Nomeado`] — uma referência
//! por nome a um tipo definido em `tipo NOME = ...`, que só existe "pelo
//! nome" até a fase de verificação semântica (seção 4.3). [`TipoResolvido`]
//! é a versão "achatada": toda referência nomeada foi substituída pela
//! definição real (`registro`, `conjunto`, primitivo ou `generico`),
//! recursivamente — [`resolver_tipo`] faz essa transformação, detectando
//! tipos não definidos e ciclos (`tipo A = B` / `tipo B = A`).
//!
//! Este módulo também define:
//! - [`compatibilidade`] — para atribuições, parâmetros, retornos, `leia`,
//!   `escreva`, `dimensione`, ramos de `caso` etc. (seção 10.5.1).
//! - [`tipo_resultado_binario`] / [`tipo_resultado_unario`] — para o tipo
//!   resultante de cada operador (seções 5.2/5.3/5.4/5.5 e 10.5.2,
//!   concatenação de `cadeia` com `+`).

use crate::ast::{OpBinario, OpUnario, Tipo, TipoPrimitivo};
use std::collections::HashMap;

// =====================================================================================
// Tipos resolvidos
// =====================================================================================

/// Um tipo PEPPE já "achatado" — sem nenhuma referência pendente a um nome
/// definido via `tipo NOME = ...` (seção 4.3/4.4/4.5).
#[derive(Debug, Clone, PartialEq)]
pub enum TipoResolvido {
    Inteiro,
    Real,
    Cadeia,
    Caractere,
    Logico,
    /// Tipo `generico` (seção 10.5, fase 2). Para esta versão do
    /// verificador, `generico` é tratado de forma permissiva: compatível
    /// com qualquer outro tipo (ver [`compatibilidade`]) — uma
    /// simplificação até que a verificação de tipos paramétricos seja
    /// implementada.
    Generico,
    Registro(Vec<CampoResolvido>),
    /// `dimensoes` permanece como na AST (`Vec<Option<(Expr, Expr)>>`):
    /// avaliar os limites exige o ambiente de execução (podem depender de
    /// `const`s ou — após `dimensione` — de valores de variáveis), então a
    /// verificação estática de tipos não precisa resolvê-los a inteiros.
    Conjunto {
        dimensoes: Vec<Option<(crate::ast::Expr, crate::ast::Expr)>>,
        elemento: Box<TipoResolvido>,
    },
    /// Tipo `classe` (seção 10.1) — diferente de `registro`/`conjunto`,
    /// usa tipagem **nominal** (duas classes com os mesmos campos não são
    /// o mesmo tipo): a igualdade e a compatibilidade dependem do `nome`,
    /// não da estrutura. `heranca` guarda os nomes das classes-base
    /// **diretas** (vazio se não houver — Fase 6, herança múltipla
    /// estilo C++ sem `virtual`) — usado por [`e_subclasse_de`] para
    /// verificar compatibilidade através da hierarquia (seção 10.4,
    /// `REF ← OBJ2` quando `REF` é de uma classe-base, direta ou
    /// indireta por qualquer caminho, e `OBJ2` é de uma classe derivada).
    Classe { nome: String, heranca: Vec<String> },
    /// Tipo `função(tipo1, tipo2, ...)` (seção 10.5.3) — referência a
    /// função de primeira classe. `parametros` são só os tipos dos
    /// parâmetros (já resolvidos); o tipo de retorno é livre e por
    /// isso não entra aqui. Compatibilidade entre dois `Funcao` exige
    /// mesma quantidade e mesmos tipos de parâmetro, na ordem (ver
    /// [`compatibilidade`]) — nomes de parâmetro não importam, só tipo.
    Funcao { parametros: Vec<TipoResolvido> },
}

/// Um campo de `registro`, já com o tipo resolvido.
#[derive(Debug, Clone, PartialEq)]
pub struct CampoResolvido {
    pub nome: String,
    pub tipo: TipoResolvido,
}

impl TipoResolvido {
    /// Nome do tipo para mensagens de erro didáticas (seção 15.3). Para
    /// `registro`/`conjunto` **anônimos** (declarados inline, sem passar por
    /// `tipo NOME = ...`), retorna uma descrição estrutural; o chamador que
    /// conhece o nome do alias original (ex.: `cad_aluno`) deve preferir
    /// usá-lo na mensagem quando disponível.
    pub fn nome_exibicao(&self) -> String {
        match self {
            TipoResolvido::Inteiro => "inteiro".to_string(),
            TipoResolvido::Real => "real".to_string(),
            TipoResolvido::Cadeia => "cadeia".to_string(),
            TipoResolvido::Caractere => "caractere".to_string(),
            TipoResolvido::Logico => "lógico".to_string(),
            TipoResolvido::Generico => "generico".to_string(),
            TipoResolvido::Registro(_) => "registro".to_string(),
            TipoResolvido::Conjunto { elemento, dimensoes } => {
                format!("conjunto [{}] de {}", "..".repeat(dimensoes.len().max(1)) , elemento.nome_exibicao())
            }
            TipoResolvido::Classe { nome, .. } => nome.clone(),
            TipoResolvido::Funcao { parametros } => {
                let tipos: Vec<String> = parametros.iter().map(|p| p.nome_exibicao()).collect();
                format!("função({})", tipos.join(", "))
            }
        }
    }
}

// =====================================================================================
// Resolução de `Tipo::Nomeado` (seção 4.3)
// =====================================================================================

/// Erro ao resolver uma referência de tipo (`Tipo::Nomeado`).
#[derive(Debug, Clone, PartialEq)]
pub enum ErroResolucaoTipo {
    /// `tipo X = ALGUM_NOME`, onde `ALGUM_NOME` não foi declarado em nenhum
    /// `tipo ALGUM_NOME = ...` (case-insensitive — seção 1.3).
    TipoNaoDefinido(String),
    /// Cadeia de aliases que se referenciam ciclicamente (ex.: `tipo A = B`
    /// / `tipo B = A`). A lista contém os nomes (grafia original) na ordem
    /// em que foram percorridos, terminando no nome que fecha o ciclo.
    CicloDeTipos(Vec<String>),
}

/// Resolve `tipo` para [`TipoResolvido`], substituindo recursivamente toda
/// referência [`Tipo::Nomeado`] pela definição correspondente em
/// `tabela_tipos` (mapa: nome em minúsculas → `(grafia original,
/// definição)` — case-insensitive, seção 1.3).
pub fn resolver_tipo(
    tipo: &Tipo,
    tabela_tipos: &HashMap<String, (String, Tipo)>,
) -> Result<TipoResolvido, ErroResolucaoTipo> {
    resolver_tipo_rec(tipo, tabela_tipos, &mut Vec::new())
}

fn resolver_tipo_rec(
    tipo: &Tipo,
    tabela_tipos: &HashMap<String, (String, Tipo)>,
    pilha: &mut Vec<String>,
) -> Result<TipoResolvido, ErroResolucaoTipo> {
    match tipo {
        Tipo::Primitivo(TipoPrimitivo::Inteiro) => Ok(TipoResolvido::Inteiro),
        Tipo::Primitivo(TipoPrimitivo::Real) => Ok(TipoResolvido::Real),
        Tipo::Primitivo(TipoPrimitivo::Cadeia) => Ok(TipoResolvido::Cadeia),
        Tipo::Primitivo(TipoPrimitivo::Caractere) => Ok(TipoResolvido::Caractere),
        Tipo::Primitivo(TipoPrimitivo::Logico) => Ok(TipoResolvido::Logico),
        Tipo::Generico => Ok(TipoResolvido::Generico),

        Tipo::Registro(campos) => {
            let mut resolvidos = Vec::new();
            for campo in campos {
                let tipo_campo = resolver_tipo_rec(&campo.tipo, tabela_tipos, pilha)?;
                for nome in &campo.nomes {
                    resolvidos.push(CampoResolvido { nome: nome.clone(), tipo: tipo_campo.clone() });
                }
            }
            Ok(TipoResolvido::Registro(resolvidos))
        }

        Tipo::Conjunto { dimensoes, elemento } => {
            let elemento_resolvido = resolver_tipo_rec(elemento, tabela_tipos, pilha)?;
            Ok(TipoResolvido::Conjunto {
                dimensoes: dimensoes.clone(),
                elemento: Box::new(elemento_resolvido),
            })
        }

        Tipo::Funcao { parametros } => {
            let mut resolvidos = Vec::new();
            for p in parametros {
                resolvidos.push(resolver_tipo_rec(p, tabela_tipos, pilha)?);
            }
            Ok(TipoResolvido::Funcao { parametros: resolvidos })
        }

        // 'classe' não passa por esta função de resolução genérica: ela
        // tem identidade nominal (precisa do nome pelo qual foi declarada
        // em 'tipo NOME = classe ...', que esta função não recebe) e
        // membros que exigem uma estrutura própria no verificador
        // semântico (`InfoClasse`, seção 10). O verificador resolve
        // `Tipo::Classe`/`Tipo::Nomeado` apontando para uma classe
        // diretamente a partir de sua tabela `info_classes`, sem passar
        // por aqui.
        Tipo::Classe { .. } => unreachable!(
            "Tipo::Classe não deveria chegar a resolver_tipo_rec — o \
             verificador resolve tipos-classe via sua própria tabela \
             'info_classes' (ver checker::Verificador)."
        ),

        Tipo::Nomeado(nome) => {
            let chave = nome.to_lowercase();
            if let Some(pos) = pilha.iter().position(|n| n == &chave) {
                let mut ciclo: Vec<String> = pilha[pos..].to_vec();
                ciclo.push(chave);
                return Err(ErroResolucaoTipo::CicloDeTipos(ciclo));
            }
            let (nome_original, definicao) = tabela_tipos
                .get(&chave)
                .ok_or_else(|| ErroResolucaoTipo::TipoNaoDefinido(nome.clone()))?;

            // Caso especial: 'NOME' aponta para uma definição 'classe'.
            // Aqui (e só aqui) temos o nome pelo qual a classe foi
            // declarada — construímos o TipoResolvido::Classe
            // diretamente, sem descer recursivamente (não há mais nada a
            // "achatar": o nome da classe-base, se houver, já é só uma
            // string). Evita cair no 'unreachable!' do branch
            // 'Tipo::Classe' abaixo, que só protege contra um
            // 'Tipo::Classe' chegando aqui por algum outro caminho
            // inesperado.
            if let Tipo::Classe { heranca, .. } = definicao {
                return Ok(TipoResolvido::Classe {
                    nome: nome_original.clone(),
                    heranca: heranca.clone(),
                });
            }

            pilha.push(chave);
            let resultado = resolver_tipo_rec(definicao, tabela_tipos, pilha);
            pilha.pop();
            resultado
        }
    }
}

// =====================================================================================
// Compatibilidade / coerção (seção 10.5.1)
// =====================================================================================

/// Resultado da verificação de compatibilidade entre um valor de tipo `de`
/// e um destino de tipo `para` — usado em atribuições, argumentos de
/// sub-rotina, retorno de função, `leia`/`escreva`, ramos de `caso`,
/// `dimensione` etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibilidade {
    /// Tipos idênticos, ou conversão implícita permitida pela tabela de
    /// coerção (seção 10.5.1) — ex.: `inteiro -> real`, `caractere -> cadeia`.
    Direta,
    /// `de` pode se tornar `para`, mas exige *cast* explícito —
    /// `(para) valor` ou `para(valor)` (seção 10.5.1).
    PrecisaCast,
    /// Nenhuma conversão é permitida entre `de` e `para`.
    Incompativel,
}

/// Tabela de coerção da seção 10.5.1:
///
/// | de \ para | inteiro | real | cadeia | caractere | lógico |
/// |---|---|---|---|---|---|
/// | inteiro | = | **direta** | cast | incompatível | incompatível |
/// | real | cast | = | cast | incompatível | incompatível |
/// | cadeia | cast | cast | = | cast | incompatível |
/// | caractere | incompatível | incompatível | **direta** | = | incompatível |
/// | lógico | incompatível | incompatível | incompatível | incompatível | = |
///
/// `generico` (✅ simplificação v1, ver [`TipoResolvido::Generico`]) é
/// compatível (direta) com qualquer tipo, nos dois sentidos.
pub fn compatibilidade(de: &TipoResolvido, para: &TipoResolvido) -> Compatibilidade {
    use Compatibilidade::*;
    use TipoResolvido::*;

    if de == para {
        return Direta;
    }

    // 'Funcao' (seção 10.5.3) não tem braço explícito abaixo: como o
    // tipo de retorno não entra em `TipoResolvido::Funcao` (só os
    // parâmetros), dois `Funcao` com os mesmos parâmetros já são
    // estruturalmente IGUAIS (`PartialEq`) e caem no `de == para`
    // acima. Dois `Funcao` com parâmetros diferentes não têm conversão
    // implícita nem explícita — caem no '_ => Incompativel' do match
    // abaixo, como esperado (não existe 'cast' de função).

    match (de, para) {
        (Generico, _) | (_, Generico) => Direta,

        (Inteiro, Real) => Direta,
        (Real, Inteiro) => PrecisaCast,

        (Caractere, Cadeia) => Direta,
        (Cadeia, Caractere) => PrecisaCast,

        (Inteiro, Cadeia) | (Cadeia, Inteiro) => PrecisaCast,
        (Real, Cadeia) | (Cadeia, Real) => PrecisaCast,

        // registro/conjunto: só compatíveis se estruturalmente idênticos
        // (registro) ou "de mesma forma" — mesmo número de dimensões e
        // elemento compatível diretamente (conjunto). Os limites exatos das
        // dimensões (Expr) não entram na comparação: `conjunto [1..8] de
        // real` e `conjunto [1..N] de real` têm a "mesma forma".
        (Conjunto { dimensoes: da, elemento: ea }, Conjunto { dimensoes: db, elemento: eb }) => {
            if da.len() == db.len() && compatibilidade(ea, eb) == Direta {
                Direta
            } else {
                Incompativel
            }
        }

        _ => Incompativel,
    }
}

/// `true` se `nome_filha` é a própria `nome_base`, ou se descende dela
/// através de **qualquer** caminho na árvore de herança (seção
/// 10.1/10.4/Fase 6 — múltiplas bases diretas por classe, sem herança
/// virtual: se duas bases diretas compartilham uma base comum mais
/// acima, cada caminho é percorrido independentemente, sem
/// deduplicação — mas para esta função isso não importa, já que ela só
/// responde sim/não, não busca um campo/método específico onde a
/// duplicação causaria ambiguidade). Comparação case-insensitive (seção
/// 1.3). `tabela_heranca` mapeia nome de classe (minúsculas) → lista de
/// nomes das classes-base diretas (vazia se não houver).
pub fn e_subclasse_de(
    nome_filha: &str,
    nome_base: &str,
    tabela_heranca: &HashMap<String, Vec<String>>,
) -> bool {
    let base = nome_base.to_lowercase();
    let mut pilha = vec![nome_filha.to_lowercase()];
    let mut visitados = std::collections::HashSet::new();
    while let Some(atual) = pilha.pop() {
        if atual == base {
            return true;
        }
        if !visitados.insert(atual.clone()) {
            // Já visitado por outro caminho (ex.: diamond problem sem
            // herança virtual) — evita reprocessar e, em caso de ciclo
            // de herança (não deveria acontecer, validado na coleta de
            // classes), evita laço infinito.
            continue;
        }
        if let Some(bases) = tabela_heranca.get(&atual) {
            for pai in bases {
                pilha.push(pai.to_lowercase());
            }
        }
    }
    false
}

/// Como [`compatibilidade`], mas também entende o caso `(Classe, Classe)`
/// usando a cadeia de herança (seção 10.4): atribuir uma instância de uma
/// classe derivada a uma variável da classe-base (ou da própria classe) é
/// `Direta`. Para qualquer outro par de tipos, delega para
/// [`compatibilidade`] sem alteração.
pub fn compatibilidade_com_heranca(
    de: &TipoResolvido,
    para: &TipoResolvido,
    tabela_heranca: &HashMap<String, Vec<String>>,
) -> Compatibilidade {
    if let (TipoResolvido::Classe { nome: nome_de, .. }, TipoResolvido::Classe { nome: nome_para, .. }) =
        (de, para)
    {
        return if e_subclasse_de(nome_de, nome_para, tabela_heranca) {
            Compatibilidade::Direta
        } else {
            Compatibilidade::Incompativel
        };
    }
    compatibilidade(de, para)
}

// =====================================================================================
// Tipo resultante de operadores (seções 5.2/5.3/5.4/5.5/10.5.2)
// =====================================================================================

fn numerico(t: &TipoResolvido) -> bool {
    matches!(t, TipoResolvido::Inteiro | TipoResolvido::Real | TipoResolvido::Generico)
}

fn textual(t: &TipoResolvido) -> bool {
    matches!(t, TipoResolvido::Cadeia | TipoResolvido::Caractere | TipoResolvido::Generico)
}

/// `inteiro` se ambos forem `inteiro`; `real` se algum dos dois for `real`
/// (promoção usual). `generico` (✅ simplificação v1) também produz
/// `generico`, para não travar a verificação antes de tipos paramétricos
/// existirem de fato.
fn numerico_resultado(a: &TipoResolvido, b: &TipoResolvido) -> TipoResolvido {
    use TipoResolvido::*;
    if *a == Generico || *b == Generico {
        Generico
    } else if *a == Real || *b == Real {
        Real
    } else {
        Inteiro
    }
}

/// Representação textual de um [`OpBinario`] para mensagens de erro (seção
/// 15.3) — sempre na forma canônica (✅ v0.9: `^` para potenciação).
pub fn simbolo_op_binario(op: OpBinario) -> &'static str {
    use OpBinario::*;
    match op {
        Soma => "+",
        Subtracao => "-",
        Multiplicacao => "*",
        Divisao => "/",
        Div => "div",
        Mod => "mod",
        Potencia => "^",
        Igual => "=",
        Diferente => "<>",
        Menor => "<",
        Maior => ">",
        MenorIgual => "<=",
        MaiorIgual => ">=",
        E => ".e.",
        Ou => ".ou.",
        Xou => ".xou.",
    }
}

/// Tipo do resultado de `esquerda <op> direita`, ou uma mensagem de erro
/// didática (seção 15.3) se a combinação de tipos não for permitida.
pub fn tipo_resultado_binario(
    op: OpBinario,
    esquerda: &TipoResolvido,
    direita: &TipoResolvido,
) -> Result<TipoResolvido, String> {
    use OpBinario::*;
    use TipoResolvido::*;

    match op {
        // '+' : soma numérica OU concatenação de cadeia/caractere (seção
        // 10.5.2) — nunca os dois tipos misturados sem cast explícito.
        Soma => {
            if numerico(esquerda) && numerico(direita) {
                Ok(numerico_resultado(esquerda, direita))
            } else if textual(esquerda) && textual(direita) {
                Ok(Cadeia)
            } else {
                Err(format!(
                    "operador '+' não está definido entre '{}' e '{}'. \
                     Para concatenar texto com um número, converta o número \
                     primeiro com 'cadeia(...)' (seção 10.5.1/10.5.2).",
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao()
                ))
            }
        }

        Subtracao | Multiplicacao => {
            if numerico(esquerda) && numerico(direita) {
                Ok(numerico_resultado(esquerda, direita))
            } else {
                Err(format!(
                    "operador '{}' exige dois operandos numéricos (inteiro/real), \
                     mas encontrei '{}' e '{}'",
                    simbolo_op_binario(op),
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao()
                ))
            }
        }

        // '/' sempre retorna 'real' (seção 5.2), mesmo entre dois 'inteiro'.
        Divisao => {
            if numerico(esquerda) && numerico(direita) {
                if *esquerda == Generico && *direita == Generico {
                    Ok(Generico)
                } else {
                    Ok(Real)
                }
            } else {
                Err(format!(
                    "operador '/' exige dois operandos numéricos (inteiro/real), \
                     mas encontrei '{}' e '{}'",
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao()
                ))
            }
        }

        // 'div'/'mod' exigem 'inteiro' dos dois lados (seção 5.2).
        Div | Mod => {
            if (*esquerda == Inteiro || *esquerda == Generico)
                && (*direita == Inteiro || *direita == Generico)
            {
                Ok(Inteiro)
            } else {
                Err(format!(
                    "operador '{}' exige dois operandos 'inteiro', mas encontrei \
                     '{}' e '{}'. Use '/' se o resultado puder ser fracionário.",
                    simbolo_op_binario(op),
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao()
                ))
            }
        }

        // '^' sempre retorna 'real' (mesma assinatura de potência(x,y) — seção 5.6).
        Potencia => {
            if numerico(esquerda) && numerico(direita) {
                if *esquerda == Generico && *direita == Generico {
                    Ok(Generico)
                } else {
                    Ok(Real)
                }
            } else {
                Err(format!(
                    "operador '^' exige dois operandos numéricos (inteiro/real), \
                     mas encontrei '{}' e '{}'",
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao()
                ))
            }
        }

        // Relacionais: numérico-com-numérico, texto-com-texto ou lógico-com-lógico.
        Igual | Diferente | Menor | Maior | MenorIgual | MaiorIgual => {
            let compativel = (numerico(esquerda) && numerico(direita))
                || (textual(esquerda) && textual(direita))
                || (*esquerda == Logico && *direita == Logico)
                || *esquerda == Generico
                || *direita == Generico;
            if compativel {
                Ok(Logico)
            } else {
                Err(format!(
                    "não é possível comparar '{}' com '{}' usando '{}' — os dois \
                     lados devem ser ambos numéricos, ambos texto (cadeia/caractere) \
                     ou ambos lógico.",
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao(),
                    simbolo_op_binario(op)
                ))
            }
        }

        // Lógicos: '.e.'/'.ou.'/'.xou.' exigem 'lógico' dos dois lados.
        E | Ou | Xou => {
            if (*esquerda == Logico || *esquerda == Generico)
                && (*direita == Logico || *direita == Generico)
            {
                Ok(Logico)
            } else {
                Err(format!(
                    "operador '{}' exige dois operandos 'lógico', mas encontrei \
                     '{}' e '{}'",
                    simbolo_op_binario(op),
                    esquerda.nome_exibicao(),
                    direita.nome_exibicao()
                ))
            }
        }
    }
}

/// Tipo do resultado de `<op> operando` (negação aritmética `-` ou lógica
/// `.não.`, seção 5.5), ou erro didático.
pub fn tipo_resultado_unario(
    op: OpUnario,
    operando: &TipoResolvido,
) -> Result<TipoResolvido, String> {
    use OpUnario::*;
    use TipoResolvido::*;

    match op {
        Negativo => {
            if numerico(operando) {
                Ok(operando.clone())
            } else {
                Err(format!(
                    "operador '-' (negação) exige um operando numérico \
                     (inteiro/real), mas encontrei '{}'",
                    operando.nome_exibicao()
                ))
            }
        }
        Nao => {
            if *operando == Logico || *operando == Generico {
                Ok(Logico)
            } else {
                Err(format!(
                    "operador '.não.' exige um operando 'lógico', mas encontrei '{}'",
                    operando.nome_exibicao()
                ))
            }
        }
    }
}

// =====================================================================================
// Testes
// =====================================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{DeclaracaoVar, Expr};

    /// Monta uma tabela de tipos a partir de pares `(nome, definição)`,
    /// normalizando a chave para minúsculas (case-insensitive — seção 1.3).
    fn tabela(defs: Vec<(&str, Tipo)>) -> HashMap<String, (String, Tipo)> {
        defs.into_iter()
            .map(|(nome, def)| (nome.to_lowercase(), (nome.to_string(), def)))
            .collect()
    }

    #[test]
    fn resolve_tipos_primitivos_e_generico() {
        let t = HashMap::new();
        assert_eq!(
            resolver_tipo(&Tipo::Primitivo(TipoPrimitivo::Inteiro), &t),
            Ok(TipoResolvido::Inteiro)
        );
        assert_eq!(resolver_tipo(&Tipo::Generico, &t), Ok(TipoResolvido::Generico));
    }

    #[test]
    fn resolve_alias_simples_case_insensitive() {
        // tipo BIMESTRE = conjunto [1..4] de real
        // var NOTAS : bimestre   (referência em minúsculas — seção 1.3)
        let t = tabela(vec![(
            "BIMESTRE",
            Tipo::Conjunto {
                dimensoes: vec![Some((Expr::Inteiro(1), Expr::Inteiro(4)))],
                elemento: Box::new(Tipo::Primitivo(TipoPrimitivo::Real)),
            },
        )]);

        let resolvido = resolver_tipo(&Tipo::Nomeado("bimestre".into()), &t).unwrap();
        match resolvido {
            TipoResolvido::Conjunto { dimensoes, elemento } => {
                assert_eq!(dimensoes.len(), 1);
                assert_eq!(*elemento, TipoResolvido::Real);
            }
            outro => panic!("esperava Conjunto, encontrei {outro:?}"),
        }
    }

    #[test]
    fn resolve_registro_com_campo_de_tipo_nomeado() {
        // tipo BIMESTRE = conjunto [1..4] de real
        // tipo CAD_ALUNO = registro NOME:cadeia  NOTAS:bimestre fim_registro
        let t = tabela(vec![
            (
                "BIMESTRE",
                Tipo::Conjunto {
                    dimensoes: vec![Some((Expr::Inteiro(1), Expr::Inteiro(4)))],
                    elemento: Box::new(Tipo::Primitivo(TipoPrimitivo::Real)),
                },
            ),
            (
                "CAD_ALUNO",
                Tipo::Registro(vec![
                    DeclaracaoVar {
                        nomes: vec!["NOME".into()],
                        tipo: Tipo::Primitivo(TipoPrimitivo::Cadeia),
                        linha: 1,
                    },
                    DeclaracaoVar {
                        nomes: vec!["NOTAS".into()],
                        tipo: Tipo::Nomeado("bimestre".into()),
                        linha: 2,
                    },
                ]),
            ),
        ]);

        let resolvido = resolver_tipo(&Tipo::Nomeado("CAD_ALUNO".into()), &t).unwrap();
        match resolvido {
            TipoResolvido::Registro(campos) => {
                assert_eq!(campos.len(), 2);
                assert_eq!(campos[0].nome, "NOME");
                assert_eq!(campos[0].tipo, TipoResolvido::Cadeia);
                assert_eq!(campos[1].nome, "NOTAS");
                assert!(matches!(campos[1].tipo, TipoResolvido::Conjunto { .. }));
            }
            outro => panic!("esperava Registro, encontrei {outro:?}"),
        }
    }

    #[test]
    fn tipo_nao_definido() {
        let t = HashMap::new();
        let erro = resolver_tipo(&Tipo::Nomeado("NAO_EXISTE".into()), &t).unwrap_err();
        assert_eq!(erro, ErroResolucaoTipo::TipoNaoDefinido("NAO_EXISTE".into()));
    }

    #[test]
    fn ciclo_de_tipos_e_detectado() {
        // tipo A = B
        // tipo B = A
        let t = tabela(vec![
            ("A", Tipo::Nomeado("B".into())),
            ("B", Tipo::Nomeado("A".into())),
        ]);
        let erro = resolver_tipo(&Tipo::Nomeado("A".into()), &t).unwrap_err();
        assert!(matches!(erro, ErroResolucaoTipo::CicloDeTipos(_)));
    }

    #[test]
    fn compatibilidade_numerica_e_textual() {
        use Compatibilidade::*;
        use TipoResolvido::*;
        assert_eq!(compatibilidade(&Inteiro, &Real), Direta);
        assert_eq!(compatibilidade(&Real, &Inteiro), PrecisaCast);
        assert_eq!(compatibilidade(&Caractere, &Cadeia), Direta);
        assert_eq!(compatibilidade(&Cadeia, &Caractere), PrecisaCast);
        assert_eq!(compatibilidade(&Inteiro, &Cadeia), PrecisaCast);
        assert_eq!(compatibilidade(&Cadeia, &Real), PrecisaCast);
        assert_eq!(compatibilidade(&Logico, &Inteiro), Incompativel);
        assert_eq!(compatibilidade(&Caractere, &Inteiro), Incompativel);
        assert_eq!(compatibilidade(&Logico, &Logico), Direta);
    }

    #[test]
    fn compatibilidade_generico_e_permissiva() {
        use Compatibilidade::Direta;
        assert_eq!(compatibilidade(&TipoResolvido::Generico, &TipoResolvido::Inteiro), Direta);
        assert_eq!(compatibilidade(&TipoResolvido::Cadeia, &TipoResolvido::Generico), Direta);
    }

    #[test]
    fn compatibilidade_conjunto_mesma_forma() {
        use Compatibilidade::*;
        let a = TipoResolvido::Conjunto {
            dimensoes: vec![Some((Expr::Inteiro(1), Expr::Inteiro(8)))],
            elemento: Box::new(TipoResolvido::Real),
        };
        let b = TipoResolvido::Conjunto {
            dimensoes: vec![Some((
                Expr::Variavel(crate::ast::LValue {
                    qualificador_base: None,
                    nome: "N".into(),
                    acessos: vec![],
                    linha: 1,
                }),
                Expr::Variavel(crate::ast::LValue {
                    qualificador_base: None,
                    nome: "M".into(),
                    acessos: vec![],
                    linha: 1,
                }),
            ))],
            elemento: Box::new(TipoResolvido::Real),
        };
        // Mesma "forma" (1 dimensão, elemento 'real') mesmo com limites
        // textualmente diferentes.
        assert_eq!(compatibilidade(&a, &b), Direta);

        let c = TipoResolvido::Conjunto {
            dimensoes: vec![Some((Expr::Inteiro(1), Expr::Inteiro(8)))],
            elemento: Box::new(TipoResolvido::Inteiro),
        };
        assert_eq!(compatibilidade(&a, &c), Incompativel);
    }

    #[test]
    fn soma_numerica_e_concatenacao() {
        use TipoResolvido::*;
        assert_eq!(tipo_resultado_binario(OpBinario::Soma, &Inteiro, &Inteiro), Ok(Inteiro));
        assert_eq!(tipo_resultado_binario(OpBinario::Soma, &Inteiro, &Real), Ok(Real));
        assert_eq!(tipo_resultado_binario(OpBinario::Soma, &Cadeia, &Caractere), Ok(Cadeia));
        assert!(tipo_resultado_binario(OpBinario::Soma, &Cadeia, &Inteiro).is_err());
    }

    #[test]
    fn divisao_sempre_real_div_mod_sempre_inteiro() {
        use TipoResolvido::*;
        assert_eq!(tipo_resultado_binario(OpBinario::Divisao, &Inteiro, &Inteiro), Ok(Real));
        assert_eq!(tipo_resultado_binario(OpBinario::Div, &Inteiro, &Inteiro), Ok(Inteiro));
        assert!(tipo_resultado_binario(OpBinario::Div, &Real, &Inteiro).is_err());
        assert!(tipo_resultado_binario(OpBinario::Mod, &Inteiro, &Real).is_err());
    }

    #[test]
    fn relacionais_e_logicos() {
        use TipoResolvido::*;
        assert_eq!(tipo_resultado_binario(OpBinario::Menor, &Inteiro, &Real), Ok(Logico));
        assert_eq!(tipo_resultado_binario(OpBinario::Igual, &Cadeia, &Caractere), Ok(Logico));
        assert!(tipo_resultado_binario(OpBinario::Menor, &Logico, &Inteiro).is_err());
        assert_eq!(tipo_resultado_binario(OpBinario::E, &Logico, &Logico), Ok(Logico));
        assert!(tipo_resultado_binario(OpBinario::Ou, &Logico, &Inteiro).is_err());
    }

    #[test]
    fn unarios() {
        use TipoResolvido::*;
        assert_eq!(tipo_resultado_unario(OpUnario::Negativo, &Real), Ok(Real));
        assert!(tipo_resultado_unario(OpUnario::Negativo, &Logico).is_err());
        assert_eq!(tipo_resultado_unario(OpUnario::Nao, &Logico), Ok(Logico));
        assert!(tipo_resultado_unario(OpUnario::Nao, &Inteiro).is_err());
    }
}
