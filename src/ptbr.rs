//! Portuguese task text, English code: accent folding, Portuguese plurals and a dev-vocabulary dictionary.
//! The Portuguese word is kept too (routes and identifiers are often Portuguese: `/compartilhar`, `pedido`).

/// `pt` (unaccented, singular) → English code words.
pub const DICT: &[(&str, &str)] = &[
    // product / business
    ("pagamento", "payment pay checkout"), ("carrinho", "cart"), ("desconto", "discount coupon"), ("cupom", "coupon"),
    ("preco", "price pricing"), ("valor", "value amount price"), ("pedido", "order"), ("compra", "purchase checkout order"),
    ("venda", "sale sell"), ("produto", "product"), ("loja", "shop store"), ("estoque", "stock inventory"),
    ("frete", "shipping freight"), ("entrega", "delivery shipping"), ("categoria", "category"), ("marca", "brand"),
    ("cliente", "customer client"), ("usuario", "user"), ("conta", "account"), ("perfil", "profile"),
    ("empresa", "company organization"), ("parceiro", "partner"), ("fornecedor", "supplier vendor"),
    ("assinatura", "subscription plan"), ("plano", "plan"), ("cobranca", "billing charge"), ("fatura", "invoice"),
    ("nota", "invoice note"), ("recibo", "receipt"), ("credito", "credit"), ("saldo", "balance"),
    ("reembolso", "refund"), ("avaliacao", "review rating"), ("comentario", "comment"), ("indicacao", "referral"),
    ("convite", "invite invitation"), ("lead", "lead"), ("contato", "contact"), ("mensagem", "message"),
    ("campanha", "campaign"), ("relatorio", "report"), ("painel", "dashboard panel"), ("metrica", "metric metrics analytics"),
    ("grafico", "chart graph"), ("anuncio", "ad ads listing"), ("imovel", "property listing asset"), ("ativo", "asset active"),
    ("agendamento", "schedule booking appointment"), ("agenda", "schedule calendar"), ("reserva", "booking reservation"),
    ("missao", "mission"), ("tarefa", "task"), ("treinamento", "training"), ("vinho", "wine"), ("racao", "food feed"),
    ("cachorro", "dog"), ("cao", "dog"), ("gato", "cat"), ("animal", "pet animal"),
    // auth / security
    ("login", "login signin auth"), ("entrar", "login signin"), ("sair", "logout signout"), ("cadastro", "signup register registration"),
    ("senha", "password"), ("autenticacao", "auth authentication"), ("permissao", "permission role"), ("acesso", "access"),
    ("seguranca", "security"), ("sessao", "session"), ("token", "token"), ("chave", "key"), ("protecao", "protection guard"),
    ("bloqueio", "block lock"), ("verificacao", "verification verify"), ("validacao", "validation validate"),
    // ui / web
    ("pagina", "page"), ("tela", "screen view page"), ("botao", "button"), ("formulario", "form"), ("campo", "field input"),
    ("menu", "menu nav"), ("navegacao", "navigation nav"), ("cabecalho", "header"), ("rodape", "footer"),
    ("barra", "bar"), ("lateral", "sidebar side"), ("modal", "modal dialog"), ("janela", "window dialog"),
    ("aviso", "warning notice alert"), ("alerta", "alert"), ("notificacao", "notification notify"), ("icone", "icon"),
    ("imagem", "image img"), ("foto", "photo image"), ("video", "video"), ("titulo", "title heading"), ("texto", "text copy"),
    ("link", "link href"), ("rota", "route"), ("busca", "search"), ("pesquisa", "search query"), ("filtro", "filter"),
    ("ordenacao", "sort order"), ("lista", "list"), ("tabela", "table"), ("cartao", "card"), ("aba", "tab"),
    ("tema", "theme"), ("escuro", "dark"), ("claro", "light"), ("cor", "color"), ("fonte", "font source"),
    ("estilo", "style css"), ("layout", "layout"), ("responsivo", "responsive mobile"), ("celular", "mobile phone"),
    ("carregamento", "loading load"), ("carregar", "load"), ("rolagem", "scroll"), ("animacao", "animation"),
    ("idioma", "language locale lang i18n"), ("traducao", "translation i18n"), ("acessibilidade", "accessibility a11y aria"),
    ("compartilhar", "share"), ("compartilhamento", "share sharing"), ("enviar", "send submit"), ("envio", "send submit upload"),
    ("salvar", "save"), ("editar", "edit"), ("excluir", "delete remove"), ("apagar", "delete remove"), ("criar", "create"),
    ("baixar", "download"), ("upload", "upload"), ("arquivo", "file"), ("pasta", "folder directory"),
    ("mapa", "map"), ("endereco", "address"), ("cidade", "city"), ("pais", "country"), ("data", "date"), ("hora", "time hour"),
    ("tempo", "time timer duration"), ("prazo", "deadline due expiry"), ("expiracao", "expiry expiration"),
    // backend / infra
    ("banco", "database db bank"), ("coluna", "column"), ("consulta", "query"), ("migracao", "migration"),
    ("servidor", "server"), ("requisicao", "request"), ("resposta", "response"), ("erro", "error"),
    ("falha", "failure error fail"), ("teste", "test spec"), ("configuracao", "config settings configuration"),
    ("ajuste", "setting settings"), ("variavel", "variable env"), ("ambiente", "environment env"), ("implantacao", "deploy deployment"),
    ("publicacao", "publish deploy"), ("fila", "queue"), ("agendador", "scheduler cron"),
    ("cache", "cache"), ("log", "log logging"), ("registro", "log record register"), ("monitoramento", "monitoring"),
    ("desempenho", "performance perf"), ("velocidade", "speed performance"), ("memoria", "memory"),
    ("integracao", "integration"), ("sincronizacao", "sync synchronization"), ("importacao", "import"), ("exportacao", "export"),
    ("rastreamento", "tracking analytics"), ("rastreio", "tracking"), ("evento", "event"), ("gatilho", "trigger hook"),
    ("permissoes", "permissions"), ("politica", "policy"), ("privacidade", "privacy"), ("termo", "terms"),
    // games / apps
    ("jogo", "game"), ("jogador", "player"), ("inimigo", "enemy"), ("fase", "level stage"), ("nivel", "level"),
    ("pontuacao", "score points"), ("ponto", "point score"), ("vida", "health life lives"), ("moeda", "coin currency"),
    ("som", "sound audio sfx"), ("musica", "music audio"), ("efeito", "effect fx"), ("colisao", "collision"),
    ("fisica", "physics"), ("camera", "camera"), ("personagem", "character player"), ("obstaculo", "obstacle"),
    ("pulo", "jump"), ("pular", "jump"), ("controle", "control controller input"), ("tecla", "key keyboard input"),
    ("teclado", "keyboard"), ("toque", "touch tap"), ("pausa", "pause"), ("reiniciar", "restart reset"), ("salvamento", "save"),
    ("conquista", "achievement"), ("ranking", "ranking leaderboard"), ("partida", "match game round"), ("rodada", "round"),
    ("dispositivo", "device"), ("aplicativo", "app application"), ("sensor", "sensor"),
    ("remoto", "remote"), ("conexao", "connection connect"), ("pareamento", "pairing pair"),
    // common verbs/nouns in task text
    ("geracao", "generation generate"), ("gerar", "generate"), ("gerado", "generated generate"), ("gerada", "generated generate"),
    ("tipo", "type types"), ("tipar", "type typed"), ("saida", "output"), ("entrada", "input entry"), ("caminho", "path"),
    ("fluxo", "flow"), ("recurso", "resource feature"), ("rotulo", "label"), ("etiqueta", "label tag"), ("dinamico", "dynamic"),
    ("dinamica", "dynamic"), ("sincronizar", "sync synchronize"), ("sincroniza", "sync synchronize"), ("referencia", "reference ref"),
    ("checar", "check"), ("verificar", "verify check"), ("validar", "validate validation"), ("cria", "create"),
    ("garrafa", "bottle"), ("paladar", "taste palate"), ("funcional", "functional"), ("visualizacao", "visualization view"),
    ("documentacao", "docs documentation"), ("auditoria", "audit"), ("definicao", "definition"), ("interacao", "interaction"),
    ("componente", "component"), ("mascote", "mascot"), ("catalogo", "catalog"), ("limpeza", "cleanup clean"),
    ("variaveis", "variables"), ("lancamento", "launch release"), ("producao", "production prod"), ("depuracao", "debug"),
    ("contador", "counter count"), ("pendente", "pending"), ("explicacao", "explanation explain"), ("comparador", "comparison compare"),
    ("comparacao", "comparison compare"), ("margem", "margin"), ("espacamento", "spacing"), ("alinhamento", "alignment align"),
];

/// Portuguese task text? Function words and accents, so English tasks keep their exact behaviour.
pub fn is_portuguese(task: &str) -> bool {
    const MARKERS: &[&str] = &[
        "o", "os", "as", "de", "do", "da", "dos", "das", "no", "na", "nos", "nas", "em", "um", "uma", "que", "para", "pra",
        "pro", "com", "sem", "não", "nao", "e", "é", "ao", "aos", "pelo", "pela", "se", "quando", "também", "está", "isso",
    ];
    let l = task.to_lowercase();
    let words = l.split(|c: char| !c.is_alphanumeric()).filter(|w| MARKERS.contains(w)).count();
    let accents = l.chars().any(|c| "çãõâêôáéíóú".contains(c));
    words + 2 * usize::from(accents) >= 2
}

pub fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'ê' | 'è' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        c => c,
    }
}

/// Portuguese plural endings to the singular the dictionary uses: notificacoes → notificacao, papeis → papel.
pub fn singular(w: &str) -> Option<String> {
    for (pl, sg) in [("coes", "cao"), ("soes", "sao"), ("oes", "ao"), ("aes", "ao"), ("ais", "al"), ("eis", "el"), ("fis", "fil"), ("res", "r"), ("zes", "z")] {
        if w.len() > pl.len() + 2 && w.ends_with(pl) {
            return Some(format!("{}{sg}", &w[..w.len() - pl.len()]));
        }
    }
    None
}

pub fn english(w: &str) -> Option<&'static str> {
    DICT.iter().find(|(pt, _)| *pt == w).map(|(_, en)| *en)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_portuguese_tasks() {
        assert!(is_portuguese("corrige o desconto do carrinho"));
        assert!(is_portuguese("adiciona validação no login"));
        assert!(!is_portuguese("fix: resolve onboarding 500 error by adding null checks to mission generation"));
        assert!(!is_portuguese("Make npm run build produce OpenNext worker for Cloudflare CI."));
    }

    #[test]
    fn folds_plurals_and_translates() {
        let w: String = "notificações".chars().map(fold).collect();
        assert_eq!(singular(&w).as_deref(), Some("notificacao"));
        assert_eq!(english("notificacao"), Some("notification notify"));
        assert_eq!(singular("papeis").as_deref(), Some("papel"));
        assert!(DICT.iter().all(|(pt, _)| pt.chars().all(|c| c.is_ascii_lowercase())));
        let mut keys: Vec<&str> = DICT.iter().map(|(k, _)| *k).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        assert_eq!(n, keys.len(), "duplicate keys in DICT");
    }
}
