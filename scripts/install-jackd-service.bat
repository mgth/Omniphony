@echo off
::
:: install-jackd-service.bat
::
:: Installe jackd (JACK2) comme service Windows via NSSM.
::
:: Pourquoi un service ?
:: --------------------
:: orender tourne en Session 0 (LocalSystem) quand il est lancé comme service
:: Windows. jackd tourne normalement en Session 1 (session interactive de
:: l'utilisateur). Ces deux sessions sont isolées : les named pipes et la
:: shared memory JACK ne sont pas partagés entre sessions, ce qui empêche
:: orender de se connecter à jackd.
::
:: En faisant tourner jackd comme service (Session 0, LocalSystem), les deux
:: process partagent la même session et le même namespace noyau → plus
:: aucun problème d'isolation.
::
:: Prérequis :
:: -----------
::   - JACK2 installé dans C:\Program Files\JACK2\
::   - NSSM (Non-Sucking Service Manager) disponible dans PATH ou à
::     l'emplacement indiqué par la variable NSSM_EXE ci-dessous
::   - Ce script doit être lancé en tant qu'Administrateur
::
:: Mode réseau (NetJACK2) :
:: ------------------------
:: jackd est configuré avec le driver "-d net" : il reçoit l'audio depuis un
:: master NetJACK2 (ex: Reaper sur le PC source) via le réseau, sans accéder
:: directement au hardware audio. Cela le rend particulièrement adapté à un
:: fonctionnement en service.
::
:: Paramètres jackd :
::   -R          : mode temps réel (priorité élevée)
::   -r 96000    : fréquence d'échantillonnage forcée à 96 kHz
::   -d net      : driver NetJACK2 (audio par réseau)
::   -n client   : nom du nœud NetJACK2
::   -C 16       : 16 canaux d'entrée (capture)
::   -P 16       : 16 canaux de sortie (playback)
::   -l 2        : latence réseau (frames de tampon)
::
:: Réinstallation :
:: ----------------
:: Ce script peut être relancé par-dessus une installation existante.
:: Il arrête et supprime automatiquement le service précédent avant
:: de le recréer avec les nouveaux paramètres.
::
:: En cas d'échec (master NetJACK2 non disponible au démarrage), NSSM
:: redémarre automatiquement jackd toutes les 5 secondes.
::

setlocal

:: Chemin vers nssm.exe - modifier si nécessaire
set NSSM_EXE=nssm.exe

:: Chemin vers jackd.exe
set JACKD_EXE=C:\Program Files\JACK2\jackd.exe

:: Nom du service Windows
set SERVICE_NAME=jackd

:: Vérification des droits administrateur
net session >nul 2>&1
if %ERRORLEVEL% neq 0 (
    echo ERREUR : Ce script doit etre execute en tant qu'Administrateur.
    pause
    exit /b 1
)

:: Vérification que jackd.exe existe
if not exist "%JACKD_EXE%" (
    echo ERREUR : jackd.exe introuvable : %JACKD_EXE%
    echo Installez JACK2 depuis https://jackaudio.org/downloads/
    pause
    exit /b 1
)

:: Recherche nssm.exe : d'abord dans le répertoire du script, puis dans le PATH
if exist "%~dp0nssm.exe" (
    set NSSM_EXE=%~dp0nssm.exe
) else (
    where nssm.exe >nul 2>&1
    if %ERRORLEVEL% neq 0 (
        echo ERREUR : nssm.exe introuvable.
        echo Telechargez NSSM depuis https://nssm.cc/download ^(version 64-bit^)
        echo et placez nssm.exe dans le meme repertoire que ce script ou dans le PATH.
        pause
        exit /b 1
    )
)

:: Supprimer l'ancienne instance du service si elle existe
sc query %SERVICE_NAME% >nul 2>&1
if %ERRORLEVEL% equ 0 (
    echo Service "%SERVICE_NAME%" existant detecte - suppression...
    sc stop %SERVICE_NAME% >nul 2>&1
    timeout /t 2 /nobreak >nul
    %NSSM_EXE% remove %SERVICE_NAME% confirm
)

echo Installation du service jackd...

%NSSM_EXE% install %SERVICE_NAME% "%JACKD_EXE%"
%NSSM_EXE% set %SERVICE_NAME% AppParameters "-R -r 96000 -d net -n client -C 16 -P 16 -l 2"
%NSSM_EXE% set %SERVICE_NAME% ObjectName LocalSystem
%NSSM_EXE% set %SERVICE_NAME% Start SERVICE_AUTO_START
%NSSM_EXE% set %SERVICE_NAME% DisplayName "JACK2 NetJACK Client"
%NSSM_EXE% set %SERVICE_NAME% Description "JACK2 en mode NetJACK2 (driver reseau). Tourne en Session 0 aux cotes d'orender pour eviter l'isolation inter-sessions Windows."
%NSSM_EXE% set %SERVICE_NAME% AppExit Default Restart
%NSSM_EXE% set %SERVICE_NAME% AppRestartDelay 5000

if %ERRORLEVEL% neq 0 (
    echo ERREUR lors de la configuration du service.
    pause
    exit /b 1
)

echo Demarrage du service...
sc start %SERVICE_NAME%

if %ERRORLEVEL% equ 0 (
    echo.
    echo Service "%SERVICE_NAME%" installe et demarre avec succes.
    echo jackd demarrera automatiquement au prochain boot.
) else (
    echo.
    echo AVERTISSEMENT : Le service a ete installe mais n'a pas pu demarrer.
    echo Verifiez que le master NetJACK2 est accessible sur le reseau.
    echo jackd reessaiera automatiquement toutes les 5 secondes.
)

pause
