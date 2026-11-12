# SimQ

En liten skrivbordsapp som håller ordning på lagets simuleringsförfrågor: vilka simuleringar som behövs, vem som gör dem, och var indata, rapport och resultat finns.

SimQ kör inga simuleringar. Själva jobbet gör du som vanligt i Ansys, SolidWorks, Fusion eller vad du nu använder. SimQ är bara kön och anslagstavlan.

Det finns ingen egen server. Appen på din dator synkar med de andras på ett av två sätt:

- **GitHub (rekommenderas):** ett privat GitHub-repo och en delad teamnyckel. Lagmedlemmarna behöver bara appen och nyckeln.
- **Delad mapp:** en vanlig mapp som synkas av t.ex. Google Drive för skrivbord.

## Så fungerar det

1. Någon lägger upp ett behov ("Ny förfrågan").
2. Någon annan ser det i kön och klickar "Ta ärendet".
3. När simuleringen är klar klickar hen "Markera klar" och fyller i var rapporten och resultaten finns, plus en kort sammanfattning.
4. Alla i laget ser samma lista och kan öppna rapport och resultat därifrån.

Appen lagrar aldrig några filer åt dig. Den sparar bara en länk eller en beskrivning av var filen ligger:

| Typ | Vad du fyller i |
| --- | --- |
| Google Drive | En länk som börjar med `http://` eller `https://` |
| Vault | En kort beskrivning, t.ex. `Uprights/v3/Assembly.iam` |
| Annat | Fri text, t.ex. `USB-stickan i verkstadslådan` |

## Kom igång med GitHub

### En person sätter upp det (en gång)

1. **Skapa ett privat repo** på GitHub, t.ex. `simq-data`. Kryssa i "Add a README file" så att repot inte är helt tomt. Använd repot bara till det här.
2. **Skapa en åtkomstnyckel** som bara gäller det repot: GitHub → Settings → Developer settings → Personal access tokens → Fine-grained tokens → Generate new token.
   - Repository access: **Only select repositories** och välj `simq-data`.
   - Permissions → Repository permissions → **Contents: Read and write**.
   - Välj ett utgångsdatum. När nyckeln går ut måste du skapa en ny och dela ut den igen.
3. **Sätt ihop teamnyckeln:** `ägare/repo:token`, till exempel `mittlag/simq-data:github_pat_…`.
4. **Dela teamnyckeln med laget** i en privat kanal.

Teamnyckeln är ett lösenord till kön. Alla som har den kan läsa och ändra allt i repot. Lägg den aldrig i ett publikt repo, på en webbsida eller i en publik chatt. GitHub spärrar dessutom nycklar som hittas i publika repon. Om nyckeln kommer på avvägar: ta bort den på GitHub och skapa en ny.

### Varje lagmedlem

1. Ladda ner appen från **Releases** och starta den (se nedan om varningen första gången).
2. Välj språk, skriv ditt visningsnamn, välj **GitHub** och klistra in teamnyckeln.
3. Klicka **Starta**.

Du behöver inget eget GitHub-konto och inget annat program.

Tips: lägger du en textfil som heter `simq-teamkey.txt` med teamnyckeln i samma mapp som appen fylls nyckeln i automatiskt vid första start. Då räcker det att skriva sitt namn.

## Kom igång med en delad mapp (alternativ)

1. Installera Google Drive för skrivbord från <https://www.google.com/drive/download/> och logga in.
2. En person skapar en mapp som heter **SimQ** i Google Drive och delar den med laget. Alla måste ha **redigeringsrätt**.
3. Alla lägger till mappen i sin Drive: öppna den under "Delas med mig", högerklicka och välj **Ordna → Lägg till genväg**. Om Drive kör i strömningsläge: högerklicka på mappen i Utforskaren/Finder och välj **Offlineåtkomst → Tillgänglig offline**.
4. Starta appen, välj **Delad mapp**, klicka "Välj mapp…" och peka ut mappen **SimQ**.

## Ladda ner och starta

Hämta den senaste versionen för ditt system under **Releases** på GitHub-sidan och packa upp:

- **Windows:** packa upp zip-filen och dubbelklicka på `SimQ.exe`.
- **macOS:** packa upp `tar.gz`-filen och dra `SimQ.app` till Program.
- **Linux:** packa upp `tar.gz`-filen och kör `./simq`.

Appen är en enda fristående fil. Ingenting behöver installeras.

Första gången väljer du språk (**Svenska** eller **English**), visningsnamn och hur appen ska synka. Varje person väljer sitt eget språk, och laget kan blanda fritt. Allt kan ändras senare under **Inställningar**.

### Appen är inte signerad

Vi har inget utvecklarcertifikat, så både Windows och macOS varnar första gången du startar appen.

**Windows (SmartScreen):** När rutan "Windows skyddade datorn" visas, klicka **Mer information** och sedan **Kör ändå**.

**macOS (Gatekeeper):** Försök öppna appen en gång (du får ett meddelande om att den inte kan öppnas). Gå sedan till **Systeminställningar → Integritet och säkerhet**, bläddra ner och klicka **Öppna ändå** bredvid SimQ. På äldre macOS räcker det att högerklicka på appen och välja **Öppna**. Du behöver bara göra det en gång.

Om det ändå inte fungerar på macOS kan du ta bort karantänmärkningen i Terminal:

```bash
xattr -dr com.apple.quarantine /Applications/SimQ.app
```

## Bra att veta

### Det tar en stund innan andras ändringar syns

Appen letar efter nyheter var 15:e sekund, när fönstret får fokus och när du klickar **Synka nu**. Med GitHub syns andras ändringar normalt inom en halv minut. Med en delad mapp behöver Drive också tid på sig, så räkna med några sekunder upp till en minut. Statusraden längst ned visar när appen senast synkade.

### "Någon hann före"

Eftersom det finns en fördröjning kan två personer hinna ta samma ärende nästan samtidigt. Då gäller den som var först. Alla appar räknar fram samma vinnare, oavsett i vilken ordning ändringarna kommer fram.

Den som förlorade får en ruta i stil med: *"Anna tog ärendet före dig, så din ändring gäller inte. Ärendet är Pågår (Anna)."* Din ändring finns kvar i ärendets historik, markerad som avvisad, men den påverkar inte ärendet.

### Utan nät

Du kan fortsätta jobba som vanligt. Ändringarna sparas på din dator och skickas när nätet är tillbaka. Med GitHub visar statusraden "Ingen kontakt med GitHub, visar sparad data" och hur många ändringar som väntar på att skickas.

Med en delad mapp: om själva mappen försvinner (t.ex. om Google Drive inte är igång) visar statusraden "Mappen hittas inte, visar sparad data". Då kan du läsa allt som redan synkats, men inte göra ändringar förrän mappen är tillbaka.

### Rör aldrig filerna i `ops/`

I repot (eller den delade mappen) finns en mapp `ops/` med en liten fil för varje händelse (skapad förfrågan, taget ärende osv.). **Redigera, flytta eller radera aldrig de filerna för hand.** Hela listan räknas fram från dem, och appen själv ändrar eller raderar aldrig en fil som redan finns. Tar du bort en fil kan ärenden försvinna eller byta status hos alla.

Det går heller inte att redigera en förfrågan i efterhand. Blev något fel: avbryt ärendet och lägg upp ett nytt.

### Säkerhetskopia

- **GitHub:** repot har full historik. Varje händelse är en egen commit, så ingenting försvinner av misstag.
- **Delad mapp:** Google Drive sparar versionshistorik och har en papperskorg. Zippa gärna hela mappen då och då och spara den någon annanstans.

### Om något ser konstigt ut

Under **Inställningar → Diagnostik** ser du antal händelser, senaste synk, hur många ändringar som väntar på att skickas, filer som inte gick att läsa och händelser som avvisats. Knappen **Bygg om cachen** tömmer appens lokala databas och läser in allt igen. Det är ofarligt: databasen är bara en kopia.

Visas "GitHub nekade åtkomst" har teamnyckeln troligen gått ut. Be den som satte upp repot om en ny och klistra in den under Inställningar.

Visas "Ny version av appen finns" eller "Uppdatera appen" har någon i laget en nyare version. Ladda ner den senaste från Releases.

## För utvecklare

Appen är skriven i Rust med [egui](https://github.com/emilk/egui). SQLite är inbyggt i binären.

```bash
cargo run --release
```

```bash
cargo test
```

Flera instanser på samma dator (för test) startas med var sin konfigurationsmapp:

```bash
cargo run --release -- --config-dir /tmp/simq-a
```

En ny release byggs automatiskt för Windows, macOS och Linux när en tagg som börjar på `v` pushas:

```bash
git tag v0.1.0 && git push origin v0.1.0
```

Lägg appens källkod i ett annat repo än lagets data, och lägg aldrig teamnyckeln i källkoden.

### Hur synken fungerar

- Varje ändring är en ny fil: `ops/ÅÅÅÅ-MM/<tid>-<räknare>_<klient>_<id>.json`. Inga filer skrivs över, så det kan aldrig bli konflikter i innehållet.
- Varje app spelar upp alla händelser i en fast ordning (hybrid logical clock, sedan klient-ID och händelse-ID) och bygger sin egen lokala SQLite-cache. Samma filer ger alltid samma resultat.
- Med GitHub arbetar appen mot en lokal kopia av `ops/` och en bakgrundstråd håller kopian och repot lika via GitHubs REST-API: nya filer i repot laddas ner, nya lokala filer laddas upp som en commit var. Teamnyckeln sparas i klartext i appens inställningsfil på datorn.
- Allt som kommer utifrån behandlas som otrustad indata: filer över 64 KiB, trasig JSON, okända fält och filer med fel namn eller sökväg ignoreras. Bara `http://`- och `https://`-länkar går att klicka på.

Koden ligger i `src/`: `model` (typer), `hlc` (klocka), `ops` (filformat), `replay` (statusmaskin), `store` (cache), `sync` (mappläsning och bakgrundstråd), `github` (synk mot repo), `config`, `i18n` (svenska/engelska) och `ui`.
