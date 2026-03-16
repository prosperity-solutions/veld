FROM nginx:alpine

COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY index.html /usr/share/nginx/html/index.html
COPY agents.html /usr/share/nginx/html/agents.html
COPY humans.html /usr/share/nginx/html/humans.html
COPY og.png /usr/share/nginx/html/og.png
COPY llms.txt /usr/share/nginx/html/llms.txt
COPY llms-full.txt /usr/share/nginx/html/llms-full.txt
COPY logo.svg /usr/share/nginx/html/logo.svg
COPY logo-wordmark.svg /usr/share/nginx/html/logo-wordmark.svg
COPY favicon.svg /usr/share/nginx/html/favicon.svg
COPY robots.txt /usr/share/nginx/html/robots.txt
COPY fonts/ /usr/share/nginx/html/fonts/

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
