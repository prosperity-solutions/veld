FROM nginx:alpine

COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY index.html /usr/share/nginx/html/index.html
COPY og.png /usr/share/nginx/html/og.png
COPY llms.txt /usr/share/nginx/html/llms.txt
COPY llms-full.txt /usr/share/nginx/html/llms-full.txt

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
